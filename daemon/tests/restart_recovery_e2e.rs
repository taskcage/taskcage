#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use taskcaged::cgroup::CgroupPaths;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const FAIL_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[test]
fn actual_restart_recovers_stale_socket_and_residual_execution() {
    if std::env::var_os("TASKCAGE_RUN_RESTART_RECOVERY_E2E").is_none() {
        eprintln!("NOT EXECUTED: 실제 Ubuntu cgroup v2 위임 환경이 필요합니다");
        return;
    }

    let daemon_bin = required_path("TASKCAGE_RESTART_RECOVERY_BIN");
    let ghost_bin = required_path("TASKCAGE_RESTART_RECOVERY_GHOST_BIN");
    let true_bin = PathBuf::from("/bin/true");
    assert!(
        true_bin.is_file(),
        "정상 완료 target /bin/true가 필요합니다"
    );

    let outer = CgroupPaths::resolve(None).expect("시험 process의 위임 cgroup root 확인");
    let nonce = unique_nonce();
    let outer_root = outer.root().to_path_buf();
    let harness = outer_root.join(format!("restart-recovery-harness-{nonce}"));
    // 실행 중인 harness는 자기 cgroup을 지울 수 없으므로 외부 systemd unit의 trap이 마지막에 회수한다.
    fs::create_dir(&harness).expect("E2E harness cgroup 생성");
    fs::write(
        harness.join("cgroup.procs"),
        format!("{}\n", std::process::id()),
    )
    .expect("E2E harness를 별도 cgroup으로 이동");
    enable_controllers(&outer_root, &["cpu", "memory", "pids"]);

    let daemon_root = outer_root.join(format!("restart-recovery-e2e-{nonce}"));
    fs::create_dir(&daemon_root).expect("실제 daemon용 전용 cgroup root 생성");
    require_controllers(&daemon_root, &["cpu", "memory", "pids"]);

    let runtime = PathBuf::from("/run").join(format!("taskcage-restart-e2e-{nonce}"));
    fs::create_dir(&runtime).expect("owner-only runtime directory 생성");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
        .expect("runtime directory mode 설정");
    let socket_path = runtime.join("taskcaged.sock");
    let first_log = runtime.join("first-taskcaged.log");
    let second_log = runtime.join("second-taskcaged.log");
    let ready_path = runtime.join("ghost.ready");

    let mut guard = RestartGuard::new(
        daemon_root.clone(),
        runtime.clone(),
        first_log.clone(),
        second_log.clone(),
    );
    guard.first = Some(spawn_daemon(
        &daemon_bin,
        &daemon_root,
        &daemon_root,
        &socket_path,
        &first_log,
    ));
    wait_for_socket(&socket_path, guard.first.as_mut().unwrap(), &first_log);

    let client_request_id = "50000000-0000-4000-8000-000000000001";
    let mut first_client = connect(&socket_path);
    let first_submit = exchange(
        &mut first_client,
        &submit_request(
            1,
            client_request_id,
            &ghost_bin,
            &["--hold-parent", ready_path.to_string_lossy().as_ref()],
            30_000,
        ),
    );
    assert_eq!(
        first_submit["type"], "taskAccepted",
        "첫 daemon RUNNING 응답: {first_submit}"
    );
    assert_eq!(first_submit["payload"]["state"], "RUNNING");
    let old_task_id = task_id(&first_submit);
    wait_for_path(&ready_path, Duration::from_secs(3), "ghost descendant 준비");
    let old_descendant_pids = read_ready_pids(&ready_path);

    let jobs_path = daemon_root.join("jobs");
    let old_job_path = jobs_path.join(format!("job-{old_task_id}"));
    assert!(old_job_path.is_dir(), "첫 RUNNING 작업 cgroup이 필요합니다");
    assert!(cgroup_populated(&old_job_path));
    assert!(all_processes_alive(&old_descendant_pids));

    let stale_metadata = fs::symlink_metadata(&socket_path).expect("첫 daemon socket 신원 확인");
    assert!(stale_metadata.file_type().is_socket());
    assert_eq!(stale_metadata.permissions().mode() & 0o777, 0o600);

    guard
        .first
        .as_mut()
        .unwrap()
        .kill()
        .expect("첫 daemon SIGKILL");
    let first_status = wait_for_exit(
        guard.first.as_mut().unwrap(),
        Instant::now() + Duration::from_secs(3),
    )
    .unwrap_or_else(|| panic!("첫 daemon SIGKILL 종료 timeout\n{}", read_log(&first_log)));
    assert_eq!(first_status.signal(), Some(libc::SIGKILL));
    drop(first_client);

    assert!(
        socket_path.exists(),
        "crash 뒤 stale socket이 남아야 합니다"
    );
    assert!(runtime.join(".taskcaged.lock").is_file());
    assert!(
        old_job_path.is_dir(),
        "crash 뒤 작업 cgroup이 남아야 합니다"
    );
    assert!(cgroup_populated(&old_job_path));
    assert!(
        all_processes_alive(&old_descendant_pids),
        "두 번째 daemon 시작 전 후손이 살아 있어야 합니다"
    );
    assert!(
        std::os::unix::net::UnixStream::connect(&socket_path).is_err(),
        "stale socket은 요청을 받을 수 없어야 합니다"
    );

    let manager_path = daemon_root.join("manager");
    assert!(
        manager_path.is_dir(),
        "재시작 process가 안전하게 진입할 기존 manager cgroup이 필요합니다"
    );
    guard.second = Some(spawn_daemon(
        &daemon_bin,
        &daemon_root,
        &manager_path,
        &socket_path,
        &second_log,
    ));
    let mut second_client = wait_for_listener_after_recovery(
        &socket_path,
        &old_job_path,
        &old_descendant_pids,
        guard.second.as_mut().unwrap(),
        &second_log,
    );

    assert!(
        !old_job_path.exists(),
        "UDS 개방 전에 잔여 job을 제거해야 합니다"
    );
    assert!(
        all_processes_gone(&old_descendant_pids),
        "UDS 개방 전에 이전 후손을 모두 종료해야 합니다"
    );
    assert_startup_log_order(&second_log);

    let capabilities = exchange(
        &mut second_client,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(20),
            "type": "getCapabilities",
            "payload": {}
        }),
    );
    assert_eq!(capabilities["type"], "capabilities");
    assert_eq!(capabilities["payload"]["cgroupV2Ready"], true);

    let missing = exchange(
        &mut second_client,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(21),
            "type": "getTask",
            "payload": { "taskId": old_task_id }
        }),
    );
    assert_eq!(missing["type"], "error", "이전 task 조회 응답: {missing}");
    assert_eq!(missing["payload"]["code"], "TASK_NOT_FOUND");

    let second_submit = exchange(
        &mut second_client,
        &submit_request(22, client_request_id, &true_bin, &[], 5_000),
    );
    assert_eq!(
        second_submit["type"], "taskAccepted",
        "재시작 뒤 같은 clientRequestId 제출 응답: {second_submit}"
    );
    let new_task_id = task_id(&second_submit);
    assert_ne!(new_task_id, old_task_id);
    assert!(!old_job_path.exists());
    assert!(all_processes_gone(&old_descendant_pids));

    let finished = poll_finished(&mut second_client, &new_task_id);
    assert_eq!(finished["state"], "FINISHED");
    assert_eq!(finished["terminationReason"], "EXITED");
    assert_eq!(finished["process"]["exitCode"], 0);
    assert_eq!(finished["process"]["signal"], Value::Null);
    assert!(!jobs_path.join(format!("job-{new_task_id}")).exists());

    drop(second_client);
    terminate_normally(guard.second.as_mut().unwrap());
    let second_status = wait_for_exit(
        guard.second.as_mut().unwrap(),
        Instant::now() + Duration::from_secs(5),
    )
    .unwrap_or_else(|| {
        panic!(
            "두 번째 daemon 정상 종료 timeout\n{}",
            read_log(&second_log)
        )
    });
    assert!(
        second_status.success(),
        "두 번째 daemon 정상 종료 코드: {second_status}\n{}",
        read_log(&second_log)
    );
    assert!(
        !socket_path.exists(),
        "정상 종료 뒤 owned socket이 남았습니다"
    );
    assert!(runtime.join(".taskcaged.lock").is_file());
    assert!(all_processes_gone(&old_descendant_pids));
    assert!(!cgroup_populated(&daemon_root));

    let cleanup_errors = guard.cleanup();
    assert!(
        cleanup_errors.is_empty(),
        "시험 guard 정리 실패: {cleanup_errors:?}"
    );
    assert!(
        !daemon_root.exists(),
        "시험용 daemon cgroup root가 남았습니다"
    );
    assert!(!runtime.exists(), "시험용 runtime directory가 남았습니다");
}

fn spawn_daemon(
    daemon_bin: &Path,
    daemon_root: &Path,
    launch_cgroup: &Path,
    socket_path: &Path,
    log_path: &Path,
) -> Child {
    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(log_path)
        .expect("daemon log 파일 생성");
    let mut command = Command::new(daemon_bin);
    command
        .arg("serve")
        .arg("--socket")
        .arg(socket_path)
        .arg("--max-concurrent-tasks")
        .arg("2")
        .arg("--cleanup-timeout-ms")
        .arg(CLEANUP_TIMEOUT.as_millis().to_string())
        .arg("--fail-stop-timeout-ms")
        .arg(FAIL_STOP_TIMEOUT.as_millis().to_string())
        .env("TASKCAGE_CGROUP_ROOT", daemon_root)
        .stdout(Stdio::from(
            log.try_clone().expect("daemon stdout log 복제"),
        ))
        .stderr(Stdio::from(log));
    move_child_before_exec(&mut command, launch_cgroup);
    command.spawn().unwrap_or_else(|error| {
        panic!(
            "실제 taskcaged serve 실행 실패: {error}\n{}",
            read_log(log_path)
        )
    })
}

fn submit_request(
    sequence: u32,
    client_request_id: &str,
    program: &Path,
    args: &[&str],
    wall_time_limit_ms: u64,
) -> Value {
    json!({
        "protocolVersion": 1,
        "requestId": request_id(sequence),
        "type": "submitTask",
        "payload": {
            "clientRequestId": client_request_id,
            "command": {
                "program": program.to_string_lossy(),
                "args": args,
                "workingDirectory": "/",
                "environment": {}
            },
            "limits": {
                "cpuMax": { "quotaMicros": 50000, "periodMicros": 100000 },
                "memoryMaxBytes": 67108864,
                "pidsMax": 8,
                "wallTimeLimitMs": wall_time_limit_ms
            },
            "output": {
                "stdoutTailMaxBytes": 1024,
                "stderrTailMaxBytes": 1024
            }
        }
    })
}

fn task_id(response: &Value) -> String {
    response["payload"]["taskId"]
        .as_str()
        .expect("응답 taskId")
        .to_owned()
}

fn move_child_before_exec(command: &mut Command, launch_cgroup: &Path) {
    let cgroup_procs = CString::new(launch_cgroup.join("cgroup.procs").as_os_str().as_bytes())
        .expect("cgroup.procs 경로에는 NUL이 없습니다");
    // 실제 daemon의 main이 시작되기 전에 이번 시작 단계가 소유할 cgroup으로 옮긴다.
    unsafe {
        command.pre_exec(move || {
            let descriptor = libc::open(
                cgroup_procs.as_ptr(),
                libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            );
            if descriptor == -1 {
                return Err(io::Error::last_os_error());
            }
            let value = b"0\n";
            let written = libc::write(descriptor, value.as_ptr().cast(), value.len());
            let write_error = io::Error::last_os_error();
            libc::close(descriptor);
            if written == value.len() as isize {
                Ok(())
            } else {
                Err(write_error)
            }
        });
    }
}

fn wait_for_socket(socket: &Path, child: &mut Child, log: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if socket.exists() {
            return;
        }
        if let Some(status) = child.try_wait().expect("daemon 상태 확인") {
            panic!(
                "socket bind 전에 daemon이 종료됐습니다: {status}\n{}",
                read_log(log)
            );
        }
        assert!(
            Instant::now() < deadline,
            "socket 시작 timeout\n{}",
            read_log(log)
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_listener_after_recovery(
    socket: &Path,
    old_job: &Path,
    old_pids: &[u32],
    child: &mut Child,
    log: &Path,
) -> std::os::unix::net::UnixStream {
    let deadline = Instant::now() + CLEANUP_TIMEOUT + Duration::from_secs(3);
    loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(socket) {
            assert!(
                !old_job.exists() && all_processes_gone(old_pids),
                "잔여 실행이 남은 동안 두 번째 daemon이 요청을 받았습니다"
            );
            configure_stream(&stream);
            return stream;
        }
        if let Some(status) = child.try_wait().expect("두 번째 daemon 상태 확인") {
            panic!(
                "시작 복구 중 두 번째 daemon이 종료됐습니다: {status}\n{}",
                read_log(log)
            );
        }
        assert!(
            Instant::now() < deadline,
            "시작 복구와 UDS bind timeout\n{}",
            read_log(log)
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn assert_startup_log_order(log_path: &Path) {
    let log = read_log(log_path);
    let recovery = log
        .find("잔여 TaskCage 작업 cgroup 복구를 완료했습니다")
        .unwrap_or_else(|| panic!("시작 복구 완료 log가 없습니다:\n{log}"));
    let preflight = log
        .find("cgroup 사전 검사를 통과했습니다")
        .unwrap_or_else(|| panic!("preflight 완료 log가 없습니다:\n{log}"));
    let started = log
        .find("TaskCage daemon started")
        .unwrap_or_else(|| panic!("daemon 시작 log가 없습니다:\n{log}"));
    assert!(
        recovery < preflight && preflight < started,
        "복구 → preflight → UDS 준비 순서가 아닙니다:\n{log}"
    );
    assert!(
        log.contains("removed_jobs=1"),
        "잔여 작업 한 건을 제거했다는 근거가 없습니다:\n{log}"
    );
}

fn poll_finished(stream: &mut std::os::unix::net::UnixStream, task_id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    for sequence in 23..100 {
        let response = exchange(
            stream,
            &json!({
                "protocolVersion": 1,
                "requestId": request_id(sequence),
                "type": "getTask",
                "payload": { "taskId": task_id }
            }),
        );
        assert_eq!(response["type"], "task", "새 작업 조회 응답: {response}");
        if response["payload"]["state"] == "FINISHED" {
            return response["payload"].clone();
        }
        assert!(Instant::now() < deadline, "새 작업 FINISHED timeout");
        thread::sleep(POLL_INTERVAL);
    }
    panic!("새 작업이 FINISHED가 되지 않았습니다")
}

fn terminate_normally(child: &mut Child) {
    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(
        result,
        0,
        "두 번째 daemon SIGTERM: {}",
        io::Error::last_os_error()
    );
}

fn connect(path: &Path) -> std::os::unix::net::UnixStream {
    let stream = std::os::unix::net::UnixStream::connect(path).expect("UDS 연결");
    configure_stream(&stream);
    stream
}

fn configure_stream(stream: &std::os::unix::net::UnixStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDS read timeout 설정");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("UDS write timeout 설정");
}

fn exchange(stream: &mut std::os::unix::net::UnixStream, request: &Value) -> Value {
    write_frame(stream, request);
    read_frame(stream)
}

fn write_frame(stream: &mut std::os::unix::net::UnixStream, value: &Value) {
    let payload = serde_json::to_vec(value).expect("protocol 요청 직렬화");
    let length = u32::try_from(payload.len()).expect("시험 frame 길이");
    stream
        .write_all(&length.to_be_bytes())
        .and_then(|()| stream.write_all(&payload))
        .expect("protocol frame 쓰기");
}

fn read_frame(stream: &mut std::os::unix::net::UnixStream) -> Value {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("protocol frame 길이 읽기");
    let length = usize::try_from(u32::from_be_bytes(prefix)).expect("frame 길이 변환");
    assert!((1..=1_048_576).contains(&length));
    let mut payload = vec![0_u8; length];
    stream
        .read_exact(&mut payload)
        .expect("protocol frame 읽기");
    serde_json::from_slice(&payload).expect("protocol 응답 JSON")
}

fn request_id(sequence: u32) -> String {
    format!("40000000-0000-4000-8000-{sequence:012}")
}

fn wait_for_path(path: &Path, timeout: Duration, label: &str) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{label} timeout: {}",
            path.display()
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_exit(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait().expect("daemon 종료 상태 확인") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn read_ready_pids(path: &Path) -> Vec<u32> {
    fs::read_to_string(path)
        .expect("ghost PID 근거 읽기")
        .lines()
        .map(|line| {
            line.split_once('=')
                .expect("ghost PID 형식")
                .1
                .parse()
                .expect("ghost PID 숫자")
        })
        .collect()
}

fn all_processes_alive(pids: &[u32]) -> bool {
    pids.iter()
        .all(|pid| Path::new(&format!("/proc/{pid}")).exists())
}

fn all_processes_gone(pids: &[u32]) -> bool {
    pids.iter()
        .all(|pid| !Path::new(&format!("/proc/{pid}")).exists())
}

fn cgroup_populated(path: &Path) -> bool {
    try_cgroup_populated(path).expect("cgroup.events 읽기")
}

fn try_cgroup_populated(path: &Path) -> io::Result<bool> {
    fs::read_to_string(path.join("cgroup.events"))?
        .lines()
        .find_map(|line| line.strip_prefix("populated "))
        .map(|value| value != "0")
        .ok_or_else(|| io::Error::other("cgroup.events에 populated 값이 없습니다"))
}

fn require_controllers(root: &Path, required: &[&str]) {
    let controllers =
        fs::read_to_string(root.join("cgroup.controllers")).expect("daemon root controller 읽기");
    for controller in required {
        assert!(
            controllers
                .split_whitespace()
                .any(|value| value == *controller),
            "daemon root에 {controller} controller가 없습니다: {controllers}"
        );
    }
}

fn enable_controllers(root: &Path, required: &[&str]) {
    require_controllers(root, required);
    let control_path = root.join("cgroup.subtree_control");
    let enabled = fs::read_to_string(&control_path).expect("상위 subtree_control 읽기");
    let missing = required
        .iter()
        .filter(|controller| {
            !enabled
                .split_whitespace()
                .any(|value| value == **controller)
        })
        .map(|controller| format!("+{controller}"))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        fs::write(&control_path, format!("{}\n", missing.join(" ")))
            .expect("daemon root에 필요한 controller 위임");
    }
    let enabled = fs::read_to_string(control_path).expect("상위 subtree_control 재확인");
    for controller in required {
        assert!(
            enabled.split_whitespace().any(|value| value == *controller),
            "{controller} controller 위임이 적용되지 않았습니다: {enabled}"
        );
    }
}

fn required_path(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} 필요")));
    assert!(path.is_absolute(), "{name}은 절대 경로여야 합니다");
    path
}

fn unique_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("현재 시각")
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| format!("log 읽기 실패: {error}"))
}

struct RestartGuard {
    first: Option<Child>,
    second: Option<Child>,
    daemon_root: PathBuf,
    runtime: PathBuf,
    first_log: PathBuf,
    second_log: PathBuf,
    cleaned: bool,
}

impl RestartGuard {
    fn new(
        daemon_root: PathBuf,
        runtime: PathBuf,
        first_log: PathBuf,
        second_log: PathBuf,
    ) -> Self {
        Self {
            first: None,
            second: None,
            daemon_root,
            runtime,
            first_log,
            second_log,
            cleaned: false,
        }
    }

    fn cleanup(&mut self) -> Vec<String> {
        if self.cleaned {
            return Vec::new();
        }
        let mut errors = Vec::new();
        if self.daemon_root.exists() {
            if let Err(error) = fs::write(self.daemon_root.join("cgroup.kill"), "1\n") {
                errors.push(format!("시험 cgroup.kill: {error}"));
            }
        }
        stop_child(self.first.as_mut());
        stop_child(self.second.as_mut());
        wait_for_cgroup_empty(&self.daemon_root, &mut errors);
        remove_cgroup_tree(&self.daemon_root, &mut errors);

        for file in [
            self.runtime.join("taskcaged.sock"),
            self.runtime.join("ghost.ready"),
            self.runtime.join(".taskcaged.lock"),
            self.first_log.clone(),
            self.second_log.clone(),
        ] {
            remove_file_if_exists(&file, &mut errors);
        }
        remove_directory_if_exists(&self.runtime, &mut errors);
        self.cleaned = true;
        errors
    }
}

impl Drop for RestartGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("첫 taskcaged log:\n{}", read_log(&self.first_log));
            eprintln!("두 번째 taskcaged log:\n{}", read_log(&self.second_log));
        }
        let errors = self.cleanup();
        if !errors.is_empty() {
            eprintln!("taskcaged restart E2E guard 오류: {errors:?}");
        }
    }
}

fn stop_child(child: Option<&mut Child>) {
    let Some(child) = child else {
        return;
    };
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_for_cgroup_empty(root: &Path, errors: &mut Vec<String>) {
    if !root.exists() {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match try_cgroup_populated(root) {
            Ok(false) => return,
            Ok(true) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
            Ok(true) => {
                errors.push("시험 cgroup populated 0 timeout".to_owned());
                return;
            }
            Err(error) => {
                errors.push(format!("시험 cgroup populated 확인: {error}"));
                return;
            }
        }
    }
}

fn remove_cgroup_tree(path: &Path, errors: &mut Vec<String>) {
    if !path.exists() {
        return;
    }
    match fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries.filter_map(Result::ok) {
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => remove_cgroup_tree(&entry.path(), errors),
                    Ok(kind) if kind.is_symlink() => errors.push(format!(
                        "시험 cgroup에 예상하지 못한 symlink가 있습니다: {}",
                        entry.path().display()
                    )),
                    Ok(_) => {}
                    Err(error) => errors.push(format!(
                        "시험 cgroup 항목 종류 확인 {}: {error}",
                        entry.path().display()
                    )),
                }
            }
        }
        Err(error) => {
            errors.push(format!("시험 cgroup 열거 {}: {error}", path.display()));
            return;
        }
    }
    remove_directory_if_exists(path, errors);
}

fn remove_file_if_exists(path: &Path, errors: &mut Vec<String>) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => errors.push(format!("{} 제거: {error}", path.display())),
    }
}

fn remove_directory_if_exists(path: &Path, errors: &mut Vec<String>) {
    match fs::remove_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => errors.push(format!("{} 제거: {error}", path.display())),
    }
}

#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use taskcaged::cgroup::CgroupPaths;

const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[test]
fn actual_serve_process_bounds_registry_without_execution_side_effects() {
    if std::env::var_os("TASKCAGE_RUN_REGISTRY_CAPACITY_E2E").is_none() {
        eprintln!("NOT EXECUTED: 실제 Ubuntu cgroup v2 위임 환경이 필요합니다");
        return;
    }

    let daemon_bin = required_path("TASKCAGE_REGISTRY_CAPACITY_BIN");
    let true_bin = required_path("TASKCAGE_REGISTRY_CAPACITY_TRUE_BIN");
    let touch_bin = required_path("TASKCAGE_REGISTRY_CAPACITY_TOUCH_BIN");
    let outer = CgroupPaths::resolve(None).expect("시험 process의 위임 cgroup root 확인");
    let nonce = unique_nonce();
    let outer_root = outer.root().to_path_buf();
    let harness = outer_root.join(format!("registry-limit-harness-{nonce}"));
    fs::create_dir(&harness).expect("E2E harness cgroup 생성");
    fs::write(
        harness.join("cgroup.procs"),
        format!("{}\n", std::process::id()),
    )
    .expect("E2E harness를 별도 cgroup으로 이동");
    enable_controllers(&outer_root, &["cpu", "memory", "pids"]);

    let daemon_root = outer_root.join(format!("registry-limit-e2e-{nonce}"));
    fs::create_dir(&daemon_root).expect("실제 daemon용 전용 cgroup root 생성");
    require_controllers(&daemon_root, &["cpu", "memory", "pids"]);
    let runtime = PathBuf::from("/run").join(format!("taskcage-registry-limit-{nonce}"));
    fs::create_dir(&runtime).expect("owner-only runtime directory 생성");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
        .expect("runtime directory mode 설정");
    let socket_path = runtime.join("taskcaged.sock");
    let log_path = runtime.join("taskcaged.log");
    let rejected_marker = runtime.join("rejected-target-started");
    let mut guard = E2eGuard::new(daemon_root.clone(), runtime.clone(), log_path.clone());

    let log = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&log_path)
        .expect("daemon log 파일 생성");
    let mut command = Command::new(&daemon_bin);
    command
        .arg("serve")
        .arg("--socket")
        .arg(&socket_path)
        .arg("--max-concurrent-tasks")
        .arg("1")
        .arg("--max-registry-tasks")
        .arg("2")
        .arg("--max-concurrent-connections")
        .arg("4")
        .arg("--cleanup-timeout-ms")
        .arg("1000")
        .arg("--fail-stop-timeout-ms")
        .arg("5000")
        .env_remove("TASKCAGE_CGROUP_ROOT")
        .stdout(Stdio::from(
            log.try_clone().expect("daemon stdout log 복제"),
        ))
        .stderr(Stdio::from(log));
    move_child_before_exec(&mut command, &daemon_root);
    guard.child = Some(command.spawn().expect("실제 taskcaged serve 실행"));
    wait_for_socket(&socket_path, guard.child.as_mut().unwrap(), &log_path);

    let capabilities = send_request(
        &socket_path,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(0),
            "type": "getCapabilities",
            "payload": {}
        }),
    );
    assert_eq!(capabilities["payload"]["maxConcurrentTasks"], 1);
    assert_eq!(capabilities["payload"].get("maxRegistryTasks"), None);

    let first_payload = submit_payload(client_request_id(1), &true_bin, Vec::new());
    let first = send_request(&socket_path, &submit_request(1, first_payload.clone()));
    assert_eq!(first["type"], "taskAccepted", "첫 submit 응답: {first}");
    let first_task_id = first["payload"]["taskId"]
        .as_str()
        .expect("첫 taskId")
        .to_owned();
    let first_finished = wait_for_finished(&socket_path, &first_task_id, 10);
    assert_eq!(first_finished["payload"]["terminationReason"], "EXITED");

    let second_payload = submit_payload(client_request_id(2), &true_bin, Vec::new());
    let second = send_request(&socket_path, &submit_request(2, second_payload));
    assert_eq!(second["type"], "taskAccepted", "둘째 submit 응답: {second}");
    let second_task_id = second["payload"]["taskId"]
        .as_str()
        .expect("둘째 taskId")
        .to_owned();
    let second_finished = wait_for_finished(&socket_path, &second_task_id, 20);
    assert_eq!(second_finished["payload"]["terminationReason"], "EXITED");
    assert_eq!(count_job_cgroups(&daemon_root.join("jobs")), 0);

    let rejected_payload = submit_payload(
        client_request_id(3),
        &touch_bin,
        vec![rejected_marker.to_string_lossy().into_owned()],
    );
    let rejected = send_request(&socket_path, &submit_request(3, rejected_payload));
    assert_eq!(rejected["type"], "error", "셋째 submit 응답: {rejected}");
    assert_eq!(rejected["payload"]["code"], "CAPACITY_EXHAUSTED");
    assert_eq!(rejected["payload"]["retryable"], true);
    assert_eq!(
        rejected["payload"]["message"],
        "task registry retention capacity is exhausted"
    );
    thread::sleep(Duration::from_millis(100));
    assert!(!rejected_marker.exists(), "거절된 target이 실행됐습니다");
    assert_eq!(count_job_cgroups(&daemon_root.join("jobs")), 0);

    let existing = send_request(&socket_path, &get_task_request(30, &first_task_id));
    assert_eq!(existing["type"], "task");
    assert_eq!(existing["payload"]["state"], "FINISHED");

    let duplicate = send_request(&socket_path, &submit_request(31, first_payload.clone()));
    assert_eq!(duplicate["type"], "task");
    assert_eq!(duplicate["payload"]["taskId"], first_task_id);

    let mut conflict_payload = first_payload;
    conflict_payload["command"]["args"] = json!(["different"]);
    let conflict = send_request(&socket_path, &submit_request(32, conflict_payload));
    assert_eq!(conflict["type"], "error");
    assert_eq!(conflict["payload"]["code"], "IDEMPOTENCY_CONFLICT");
    assert_eq!(count_job_cgroups(&daemon_root.join("jobs")), 0);

    let daemon_pid = i32::try_from(guard.child.as_ref().unwrap().id()).expect("daemon PID 변환");
    assert_eq!(unsafe { libc::kill(daemon_pid, libc::SIGTERM) }, 0);
    let status = wait_for_exit(guard.child.as_mut().unwrap(), Duration::from_secs(5))
        .unwrap_or_else(|| panic!("정상 shutdown timeout\n{}", read_log(&log_path)));
    assert!(
        status.success(),
        "정상 shutdown 실패: {status}\n{}",
        read_log(&log_path)
    );
    assert!(
        !socket_path.exists(),
        "정상 shutdown 뒤 socket이 남았습니다"
    );

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

fn submit_payload(client_request_id: String, program: &Path, args: Vec<String>) -> Value {
    json!({
        "clientRequestId": client_request_id,
        "command": {
            "program": program,
            "args": args,
            "workingDirectory": "/",
            "environment": {}
        },
        "limits": {
            "cpuMax": { "quotaMicros": 50000, "periodMicros": 100000 },
            "memoryMaxBytes": 67108864,
            "pidsMax": 8,
            "wallTimeLimitMs": 5000
        },
        "output": {
            "stdoutTailMaxBytes": 64,
            "stderrTailMaxBytes": 64
        }
    })
}

fn submit_request(sequence: u32, payload: Value) -> Value {
    json!({
        "protocolVersion": 1,
        "requestId": request_id(sequence),
        "type": "submitTask",
        "payload": payload
    })
}

fn get_task_request(sequence: u32, task_id: &str) -> Value {
    json!({
        "protocolVersion": 1,
        "requestId": request_id(sequence),
        "type": "getTask",
        "payload": { "taskId": task_id }
    })
}

fn wait_for_finished(socket: &Path, task_id: &str, request_sequence: u32) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let response = send_request(socket, &get_task_request(request_sequence, task_id));
        if response["type"] == "task" && response["payload"]["state"] == "FINISHED" {
            return response;
        }
        assert!(
            Instant::now() < deadline,
            "FINISHED polling timeout: {response}"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn send_request(socket: &Path, value: &Value) -> Value {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("UDS 연결");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDS read timeout 설정");
    let payload = serde_json::to_vec(value).expect("protocol 요청 직렬화");
    let length = u32::try_from(payload.len()).expect("시험 frame 길이");
    stream
        .write_all(&length.to_be_bytes())
        .expect("frame prefix 쓰기");
    stream.write_all(&payload).expect("frame payload 쓰기");

    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("response prefix 읽기");
    let length = usize::try_from(u32::from_be_bytes(prefix)).expect("response 길이 변환");
    assert!((1..=1_048_576).contains(&length));
    let mut response = vec![0_u8; length];
    stream
        .read_exact(&mut response)
        .expect("response payload 읽기");
    serde_json::from_slice(&response).expect("response JSON 파싱")
}

fn move_child_before_exec(command: &mut Command, daemon_root: &Path) {
    let cgroup_procs = CString::new(daemon_root.join("cgroup.procs").as_os_str().as_bytes())
        .expect("cgroup.procs 경로에는 NUL이 없습니다");
    // daemon main보다 먼저 시험 전용 cgroup으로 옮겨 harness와 실행 소유권을 분리한다.
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

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
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

fn count_job_cgroups(jobs: &Path) -> usize {
    fs::read_dir(jobs)
        .expect("jobs cgroup 열거")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
        .count()
}

fn require_controllers(root: &Path, required: &[&str]) {
    let controllers = fs::read_to_string(root.join("cgroup.controllers")).expect("controller 읽기");
    for controller in required {
        assert!(
            controllers
                .split_whitespace()
                .any(|value| value == *controller),
            "{controller} controller가 없습니다: {controllers}"
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
            .expect("필요한 controller 위임");
    }
}

fn cgroup_populated(path: &Path) -> io::Result<bool> {
    fs::read_to_string(path.join("cgroup.events"))?
        .lines()
        .find_map(|line| line.strip_prefix("populated "))
        .map(|value| value != "0")
        .ok_or_else(|| io::Error::other("cgroup.events에 populated 값이 없습니다"))
}

fn request_id(sequence: u32) -> String {
    format!("72000000-0000-4000-8000-{sequence:012}")
}

fn client_request_id(sequence: u32) -> String {
    format!("73000000-0000-4000-8000-{sequence:012}")
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

struct E2eGuard {
    child: Option<Child>,
    daemon_root: PathBuf,
    runtime: PathBuf,
    log: PathBuf,
    cleaned: bool,
}

impl E2eGuard {
    fn new(daemon_root: PathBuf, runtime: PathBuf, log: PathBuf) -> Self {
        Self {
            child: None,
            daemon_root,
            runtime,
            log,
            cleaned: false,
        }
    }

    fn cleanup(&mut self) -> Vec<String> {
        if self.cleaned {
            return Vec::new();
        }
        let mut errors = Vec::new();
        if self.daemon_root.exists() {
            let _ = fs::write(self.daemon_root.join("cgroup.kill"), "1\n");
        }
        if let Some(child) = self.child.as_mut()
            && wait_for_exit(child, Duration::from_secs(2)).is_none()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        if self.daemon_root.exists() {
            let deadline = Instant::now() + Duration::from_secs(2);
            while cgroup_populated(&self.daemon_root).unwrap_or(false) && Instant::now() < deadline
            {
                thread::sleep(POLL_INTERVAL);
            }
        }
        let jobs = self.daemon_root.join("jobs");
        if let Ok(entries) = fs::read_dir(&jobs) {
            for entry in entries.filter_map(Result::ok) {
                if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                    remove_directory_if_exists(&entry.path(), &mut errors);
                }
            }
        }
        for path in [
            jobs,
            self.daemon_root.join("manager"),
            self.daemon_root.clone(),
        ] {
            remove_directory_if_exists(&path, &mut errors);
        }
        for path in [
            self.runtime.join("taskcaged.sock"),
            self.runtime.join(".taskcaged.lock"),
            self.runtime.join("rejected-target-started"),
            self.log.clone(),
        ] {
            remove_file_if_exists(&path, &mut errors);
        }
        remove_directory_if_exists(&self.runtime, &mut errors);
        self.cleaned = true;
        errors
    }
}

impl Drop for E2eGuard {
    fn drop(&mut self) {
        if std::thread::panicking() && self.log.exists() {
            eprintln!("taskcaged Registry limit E2E log:\n{}", read_log(&self.log));
        }
        let errors = self.cleanup();
        if !errors.is_empty() {
            eprintln!("taskcaged Registry limit E2E guard 오류: {errors:?}");
        }
    }
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

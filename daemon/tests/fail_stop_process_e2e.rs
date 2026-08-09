#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use taskcaged::cgroup::CgroupPaths;

const CLEANUP_TIMEOUT: Duration = Duration::from_millis(300);
const FAIL_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

#[test]
fn actual_serve_process_exits_nonzero_after_fail_stop_deadline() {
    if std::env::var_os("TASKCAGE_RUN_FAIL_STOP_PROCESS_E2E").is_none() {
        eprintln!("NOT EXECUTED: 실제 Ubuntu cgroup v2 위임 환경이 필요합니다");
        return;
    }

    let daemon_bin = required_path("TASKCAGE_FAIL_STOP_PROCESS_BIN");
    let ghost_bin = required_path("TASKCAGE_FAIL_STOP_PROCESS_GHOST_BIN");
    let outer = CgroupPaths::resolve(None).expect("시험 process의 위임 cgroup root 확인");
    let nonce = unique_nonce();
    let outer_root = outer.root().to_path_buf();
    let harness = outer_root.join(format!("fail-stop-process-harness-{nonce}"));
    // 실행 중인 harness는 자기 cgroup을 지울 수 없으므로 외부 systemd unit의 trap이 마지막에 회수한다.
    fs::create_dir(&harness).expect("E2E harness cgroup 생성");
    fs::write(
        harness.join("cgroup.procs"),
        format!("{}\n", std::process::id()),
    )
    .expect("E2E harness를 별도 cgroup으로 이동");
    enable_controllers(&outer_root, &["cpu", "memory", "pids"]);
    let daemon_root = outer.root().join(format!("fail-stop-process-e2e-{nonce}"));
    fs::create_dir(&daemon_root).expect("실제 daemon용 전용 cgroup root 생성");
    require_controllers(&daemon_root, &["cpu", "memory", "pids"]);

    let runtime = PathBuf::from("/run").join(format!("taskcage-fail-stop-e2e-{nonce}"));
    fs::create_dir(&runtime).expect("owner-only runtime directory 생성");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
        .expect("runtime directory mode 설정");
    let socket_path = runtime.join("taskcaged.sock");
    let log_path = runtime.join("taskcaged.log");
    let ready_path = runtime.join("ghost.ready");

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
        .arg("2")
        .arg("--max-registry-tasks")
        .arg("16")
        .arg("--max-concurrent-connections")
        .arg("8")
        .arg("--cleanup-timeout-ms")
        .arg(CLEANUP_TIMEOUT.as_millis().to_string())
        .arg("--fail-stop-timeout-ms")
        .arg(FAIL_STOP_TIMEOUT.as_millis().to_string())
        .args([
            "--max-task-cpu-quota-us",
            "200000",
            "--max-task-cpu-period-us",
            "100000",
            "--max-task-memory-bytes",
            "2147483648",
            "--max-task-pids",
            "128",
            "--max-task-timeout-ms",
            "900000",
            "--max-task-stdout-tail-bytes",
            "65536",
            "--max-task-stderr-tail-bytes",
            "65536",
        ])
        .env_remove("TASKCAGE_CGROUP_ROOT")
        .stdout(Stdio::from(
            log.try_clone().expect("daemon stdout log 복제"),
        ))
        .stderr(Stdio::from(log));
    move_child_before_exec(&mut command, &daemon_root);
    guard.child = Some(command.spawn().expect("실제 taskcaged serve 실행"));

    wait_for_socket(&socket_path, guard.child.as_mut().unwrap(), &log_path);
    let mut observer = connect(&socket_path);
    let mut existing_submitter = connect(&socket_path);

    let submit = exchange(
        &mut observer,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(1),
            "type": "submitTask",
            "payload": {
                "clientRequestId": "10000000-0000-4000-8000-000000000001",
                "command": {
                    "program": ghost_bin.to_string_lossy(),
                    "args": ["--hold-parent", ready_path.to_string_lossy()],
                    "workingDirectory": "/",
                    "environment": {}
                },
                "limits": {
                    "cpuMax": { "quotaMicros": 50000, "periodMicros": 100000 },
                    "memoryMaxBytes": 67108864,
                    "pidsMax": 8,
                    "wallTimeLimitMs": 30000
                },
                "output": {
                    "stdoutTailMaxBytes": 1024,
                    "stderrTailMaxBytes": 1024
                }
            }
        }),
    );
    assert_eq!(submit["type"], "taskAccepted", "RUNNING 응답: {submit}");
    assert_eq!(submit["payload"]["state"], "RUNNING");
    let task_id = submit["payload"]["taskId"]
        .as_str()
        .expect("taskAccepted.taskId")
        .to_owned();
    wait_for_path(&ready_path, Duration::from_secs(3), "ghost descendant 준비");
    let descendant_pids = read_ready_pids(&ready_path);

    let jobs_path = daemon_root.join("jobs");
    let job_path = jobs_path.join(format!("job-{task_id}"));
    assert!(job_path.is_dir(), "RUNNING 작업 cgroup이 필요합니다");
    let blocker = job_path.join("e2e-removal-blocker");
    fs::create_dir(&blocker).expect("작업 cgroup 제거를 막는 빈 하위 cgroup 생성");
    guard.blocker = Some(blocker.clone());
    let jobs_before_rejection = count_job_cgroups(&jobs_path);
    assert_eq!(jobs_before_rejection, 1);

    let mut cancellation = connect(&socket_path);
    write_frame(
        &mut cancellation,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(2),
            "type": "cancelTask",
            "payload": { "taskId": task_id }
        }),
    );
    drop(cancellation);

    let fail_stop_observed = wait_for_fail_stop_capability(&mut observer);
    let running = exchange(
        &mut observer,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(20),
            "type": "getTask",
            "payload": { "taskId": task_id }
        }),
    );
    assert_eq!(running["type"], "task", "기존 연결 작업 조회: {running}");
    assert_eq!(running["payload"]["state"], "RUNNING");

    let rejected = exchange(
        &mut existing_submitter,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(21),
            "type": "submitTask",
            "payload": {
                "clientRequestId": "20000000-0000-4000-8000-000000000002",
                "command": {
                    "program": ghost_bin.to_string_lossy(),
                    "args": [],
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
            }
        }),
    );
    assert_eq!(
        rejected["type"], "error",
        "fail-stop submit 응답: {rejected}"
    );
    assert_eq!(rejected["payload"]["code"], "ENVIRONMENT_UNAVAILABLE");
    assert_eq!(count_job_cgroups(&jobs_path), jobs_before_rejection);

    wait_for_new_connections_to_stop(&socket_path, guard.child.as_mut().unwrap());
    assert!(
        socket_path.exists(),
        "프로세스 종료 전 listener만 닫혀야 합니다"
    );

    let status = wait_for_exit(
        guard.child.as_mut().unwrap(),
        fail_stop_observed + FAIL_STOP_TIMEOUT + Duration::from_secs(2),
    )
    .unwrap_or_else(|| panic!("fail-stop 종료 기한을 넘었습니다\n{}", read_log(&log_path)));
    let elapsed = fail_stop_observed.elapsed();
    assert!(!status.success(), "fail-stop은 0 코드로 종료하면 안 됩니다");
    assert!(
        elapsed <= FAIL_STOP_TIMEOUT + Duration::from_secs(1),
        "fail-stop deadline을 초과했습니다: elapsed={elapsed:?}\n{}",
        read_log(&log_path)
    );
    let log = read_log(&log_path);
    assert!(
        log.contains("fail-stop"),
        "실제 daemon log에 fail-stop 근거가 없습니다:\n{log}"
    );
    assert!(
        !socket_path.exists(),
        "fail-stop 종료에서 owned socket을 정리해야 합니다"
    );
    assert!(
        !cgroup_populated(&job_path),
        "작업 cgroup에 process가 남았습니다"
    );
    assert!(
        descendant_pids
            .iter()
            .all(|pid| !Path::new(&format!("/proc/{pid}")).exists()),
        "ghost descendant가 남았습니다: {descendant_pids:?}"
    );

    drop(observer);
    drop(existing_submitter);
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

#[test]
fn actual_shutdown_drain_switches_to_fail_stop_and_exits_nonzero() {
    if std::env::var_os("TASKCAGE_RUN_FAIL_STOP_PROCESS_E2E").is_none() {
        eprintln!("NOT EXECUTED: 실제 Ubuntu cgroup v2 위임 환경이 필요합니다");
        return;
    }

    let daemon_bin = required_path("TASKCAGE_FAIL_STOP_PROCESS_BIN");
    let ghost_bin = required_path("TASKCAGE_FAIL_STOP_PROCESS_GHOST_BIN");
    let outer = CgroupPaths::resolve(None).expect("시험 process의 위임 cgroup root 확인");
    let nonce = unique_nonce();
    let outer_root = outer.root().to_path_buf();
    let harness = outer_root.join(format!("shutdown-fail-stop-harness-{nonce}"));
    fs::create_dir(&harness).expect("E2E harness cgroup 생성");
    fs::write(
        harness.join("cgroup.procs"),
        format!("{}\n", std::process::id()),
    )
    .expect("E2E harness를 별도 cgroup으로 이동");
    enable_controllers(&outer_root, &["cpu", "memory", "pids"]);
    let daemon_root = outer.root().join(format!("shutdown-fail-stop-e2e-{nonce}"));
    fs::create_dir(&daemon_root).expect("실제 daemon용 전용 cgroup root 생성");
    require_controllers(&daemon_root, &["cpu", "memory", "pids"]);

    let runtime = PathBuf::from("/run").join(format!("taskcage-shutdown-fail-stop-{nonce}"));
    fs::create_dir(&runtime).expect("owner-only runtime directory 생성");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
        .expect("runtime directory mode 설정");
    let socket_path = runtime.join("taskcaged.sock");
    let log_path = runtime.join("taskcaged.log");
    let ready_path = runtime.join("ghost.ready");

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
        .arg("16")
        .arg("--max-concurrent-connections")
        .arg("8")
        .arg("--cleanup-timeout-ms")
        .arg(CLEANUP_TIMEOUT.as_millis().to_string())
        .arg("--fail-stop-timeout-ms")
        .arg(FAIL_STOP_TIMEOUT.as_millis().to_string())
        .args([
            "--max-task-cpu-quota-us",
            "200000",
            "--max-task-cpu-period-us",
            "100000",
            "--max-task-memory-bytes",
            "2147483648",
            "--max-task-pids",
            "128",
            "--max-task-timeout-ms",
            "900000",
            "--max-task-stdout-tail-bytes",
            "65536",
            "--max-task-stderr-tail-bytes",
            "65536",
        ])
        .env_remove("TASKCAGE_CGROUP_ROOT")
        .stdout(Stdio::from(
            log.try_clone().expect("daemon stdout log 복제"),
        ))
        .stderr(Stdio::from(log));
    move_child_before_exec(&mut command, &daemon_root);
    guard.child = Some(command.spawn().expect("실제 taskcaged serve 실행"));

    wait_for_socket(&socket_path, guard.child.as_mut().unwrap(), &log_path);
    let mut submitter = connect(&socket_path);
    let submit = exchange(
        &mut submitter,
        &json!({
            "protocolVersion": 1,
            "requestId": request_id(30),
            "type": "submitTask",
            "payload": {
                "clientRequestId": "40000000-0000-4000-8000-000000000004",
                "command": {
                    "program": ghost_bin.to_string_lossy(),
                    "args": ["--hold-parent", ready_path.to_string_lossy()],
                    "workingDirectory": "/",
                    "environment": {}
                },
                "limits": {
                    "cpuMax": { "quotaMicros": 50000, "periodMicros": 100000 },
                    "memoryMaxBytes": 67108864,
                    "pidsMax": 8,
                    "wallTimeLimitMs": 1500
                },
                "output": {
                    "stdoutTailMaxBytes": 1024,
                    "stderrTailMaxBytes": 1024
                }
            }
        }),
    );
    assert_eq!(submit["type"], "taskAccepted", "RUNNING 응답: {submit}");
    assert_eq!(submit["payload"]["state"], "RUNNING");
    let task_id = submit["payload"]["taskId"]
        .as_str()
        .expect("taskAccepted.taskId")
        .to_owned();
    wait_for_path(&ready_path, Duration::from_secs(3), "ghost descendant 준비");
    let descendant_pids = read_ready_pids(&ready_path);
    let job_path = daemon_root.join("jobs").join(format!("job-{task_id}"));
    assert!(job_path.is_dir(), "RUNNING 작업 cgroup이 필요합니다");
    let blocker = job_path.join("e2e-removal-blocker");
    fs::create_dir(&blocker).expect("작업 cgroup 제거를 막는 빈 하위 cgroup 생성");
    guard.blocker = Some(blocker);

    let daemon_pid = i32::try_from(guard.child.as_ref().unwrap().id()).expect("daemon PID 변환");
    assert_eq!(unsafe { libc::kill(daemon_pid, libc::SIGTERM) }, 0);
    let shutdown_observed = wait_for_log_contains(
        &log_path,
        "정상 shutdown drain을 시작합니다",
        Duration::from_secs(2),
        guard.child.as_mut().unwrap(),
    );
    wait_for_new_connections_to_stop(&socket_path, guard.child.as_mut().unwrap());
    assert!(
        guard.child.as_mut().unwrap().try_wait().unwrap().is_none(),
        "정상 shutdown drain 중에는 작업 cleanup을 기다려야 합니다"
    );

    let fail_stop_observed = wait_for_log_contains(
        &log_path,
        "process-wide fail-stop을 시작합니다",
        Duration::from_secs(3),
        guard.child.as_mut().unwrap(),
    );
    assert!(
        fail_stop_observed >= shutdown_observed,
        "정상 shutdown 선택 뒤에 fail-stop이 발생해야 합니다"
    );
    let status = wait_for_exit(
        guard.child.as_mut().unwrap(),
        fail_stop_observed + FAIL_STOP_TIMEOUT + Duration::from_secs(2),
    )
    .unwrap_or_else(|| {
        panic!(
            "shutdown 뒤 fail-stop 종료 기한을 넘었습니다\n{}",
            read_log(&log_path)
        )
    });
    let elapsed = fail_stop_observed.elapsed();
    assert!(
        !status.success(),
        "shutdown 뒤 fail-stop은 0 코드로 종료하면 안 됩니다"
    );
    assert!(
        elapsed <= FAIL_STOP_TIMEOUT + Duration::from_secs(1),
        "기존 fail-stop deadline을 초과했습니다: elapsed={elapsed:?}\n{}",
        read_log(&log_path)
    );

    let log = read_log(&log_path);
    let shutdown_index = log
        .find("정상 shutdown drain을 시작합니다")
        .expect("정상 shutdown drain 로그");
    let fail_stop_index = log
        .find("process-wide fail-stop을 시작합니다")
        .expect("fail-stop 전환 로그");
    assert!(
        shutdown_index < fail_stop_index,
        "shutdown이 fail-stop보다 먼저여야 합니다"
    );
    assert!(
        !socket_path.exists(),
        "정상 shutdown에서 owned socket을 정리해야 합니다"
    );
    assert_daemon_lock_released(&runtime.join(".taskcaged.lock"));
    assert!(
        !cgroup_populated(&job_path),
        "작업 cgroup에 process가 남았습니다"
    );
    assert!(
        descendant_pids
            .iter()
            .all(|pid| !Path::new(&format!("/proc/{pid}")).exists()),
        "ghost descendant가 남았습니다: {descendant_pids:?}"
    );

    drop(submitter);
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

fn move_child_before_exec(command: &mut Command, daemon_root: &Path) {
    let cgroup_procs = CString::new(daemon_root.join("cgroup.procs").as_os_str().as_bytes())
        .expect("cgroup.procs 경로에는 NUL이 없습니다");
    // 실제 daemon의 main이 시작되기 전에 시험 전용 위임 root로 옮겨 parent harness와 분리한다.
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

fn wait_for_fail_stop_capability(stream: &mut std::os::unix::net::UnixStream) -> Instant {
    let deadline = Instant::now() + Duration::from_secs(2);
    for sequence in 3..20 {
        let response = exchange(
            stream,
            &json!({
                "protocolVersion": 1,
                "requestId": request_id(sequence),
                "type": "getCapabilities",
                "payload": {}
            }),
        );
        assert_eq!(
            response["type"], "capabilities",
            "capability 응답: {response}"
        );
        if response["payload"]["cgroupV2Ready"] == false {
            return Instant::now();
        }
        assert!(
            Instant::now() < deadline,
            "fail-stop capability 전환 timeout"
        );
        thread::sleep(POLL_INTERVAL);
    }
    panic!("fail-stop capability를 확인하지 못했습니다")
}

fn wait_for_new_connections_to_stop(socket: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match std::os::unix::net::UnixStream::connect(socket) {
            Ok(stream) => drop(stream),
            Err(_) => {
                assert!(
                    child.try_wait().expect("daemon 상태 확인").is_none(),
                    "신규 연결 차단은 daemon 종료 전에 확인해야 합니다"
                );
                return;
            }
        }
        assert!(Instant::now() < deadline, "fail-stop listener 종료 timeout");
        thread::sleep(POLL_INTERVAL);
    }
}

fn connect(path: &Path) -> std::os::unix::net::UnixStream {
    let stream = std::os::unix::net::UnixStream::connect(path).expect("UDS 연결");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("UDS read timeout 설정");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("UDS write timeout 설정");
    stream
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
    format!("30000000-0000-4000-8000-{sequence:012}")
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

fn wait_for_log_contains(
    log: &Path,
    expected: &str,
    timeout: Duration,
    child: &mut Child,
) -> Instant {
    let deadline = Instant::now() + timeout;
    loop {
        if read_log(log).contains(expected) {
            return Instant::now();
        }
        if let Some(status) = child.try_wait().expect("daemon 상태 확인") {
            panic!(
                "{expected:?} 로그 전에 daemon이 종료됐습니다: {status}\n{}",
                read_log(log)
            );
        }
        assert!(
            Instant::now() < deadline,
            "{expected:?} 로그 timeout\n{}",
            read_log(log)
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn assert_daemon_lock_released(path: &Path) {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("daemon lock 파일 열기");
    let acquired = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(
        acquired, 0,
        "daemon 종료 뒤 생존 기간 lock이 해제돼야 합니다"
    );
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
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

fn count_job_cgroups(jobs: &Path) -> usize {
    fs::read_dir(jobs)
        .expect("jobs cgroup 열거")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("job-"))
        .count()
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

struct E2eGuard {
    child: Option<Child>,
    daemon_root: PathBuf,
    runtime: PathBuf,
    log: PathBuf,
    blocker: Option<PathBuf>,
    cleaned: bool,
}

impl E2eGuard {
    fn new(daemon_root: PathBuf, runtime: PathBuf, log: PathBuf) -> Self {
        Self {
            child: None,
            daemon_root,
            runtime,
            log,
            blocker: None,
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
        if let Some(child) = self.child.as_mut() {
            let deadline = Instant::now() + Duration::from_secs(2);
            if wait_for_exit(child, deadline).is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if self.daemon_root.exists() {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match try_cgroup_populated(&self.daemon_root) {
                    Ok(false) => break,
                    Ok(true) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                    Ok(true) => {
                        errors.push("시험 cgroup populated 0 timeout".to_owned());
                        break;
                    }
                    Err(error) => {
                        errors.push(format!("시험 cgroup populated 확인: {error}"));
                        break;
                    }
                }
            }
        }
        if let Some(blocker) = &self.blocker {
            remove_directory_if_exists(blocker, &mut errors);
        }
        let jobs = self.daemon_root.join("jobs");
        if let Ok(entries) = fs::read_dir(&jobs) {
            for entry in entries.filter_map(Result::ok) {
                if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                    if let Ok(children) = fs::read_dir(entry.path()) {
                        for child in children.filter_map(Result::ok) {
                            if child.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                                remove_directory_if_exists(&child.path(), &mut errors);
                            }
                        }
                    }
                    remove_directory_if_exists(&entry.path(), &mut errors);
                }
            }
        }
        remove_directory_if_exists(&jobs, &mut errors);
        remove_directory_if_exists(&self.daemon_root.join("manager"), &mut errors);
        remove_directory_if_exists(&self.daemon_root, &mut errors);

        for file in [
            self.runtime.join("taskcaged.sock"),
            self.runtime.join("ghost.ready"),
            self.runtime.join(".taskcaged.lock"),
            self.log.clone(),
        ] {
            remove_file_if_exists(&file, &mut errors);
        }
        remove_directory_if_exists(&self.runtime, &mut errors);
        self.cleaned = true;
        errors
    }
}

impl Drop for E2eGuard {
    fn drop(&mut self) {
        if std::thread::panicking() && self.log.exists() {
            eprintln!("taskcaged fail-stop E2E log:\n{}", read_log(&self.log));
        }
        let errors = self.cleanup();
        if !errors.is_empty() {
            eprintln!("taskcaged fail-stop E2E guard 오류: {errors:?}");
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

use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;

const SAFETY_TIMEOUT: Duration = Duration::from_secs(30);

fn main() {
    let mut args = env::args_os().skip(1);
    match args.next().as_deref() {
        Some(value) if value == "--child" => child(args.next().map(PathBuf::from)),
        Some(value) if value == "--hold-parent" => {
            parent(args.next().map(PathBuf::from));
        }
        Some(value) if value == "--grandchild" => thread::sleep(SAFETY_TIMEOUT),
        _ => parent(None),
    }
}

// 대표 프로세스가 먼저 끝난 뒤에도 제한된 자식 하나를 일부러 남긴다.
// TaskCage가 대표 PID만이 아니라 작업 cgroup 전체를 정리하는지 확인하기 위한 동작이다.
#[allow(clippy::zombie_processes)]
fn parent(ready_path: Option<PathBuf>) {
    let executable = env::current_exe().expect("시험 프로그램 경로 확인");
    let mut command = Command::new(executable);
    command.arg("--child");
    if let Some(path) = &ready_path {
        command.arg(path);
    }
    let child = command.spawn().expect("제한된 자식 프로세스 생성");
    println!("시험용 자식 PID={}", child.id());
    if ready_path.is_some() {
        thread::sleep(SAFETY_TIMEOUT);
    } else {
        thread::sleep(Duration::from_millis(200));
    }
}

// 자식은 손자 하나만 만들고 최대 30초 뒤에는 스스로 끝난다.
#[allow(clippy::zombie_processes)]
fn child(ready_path: Option<PathBuf>) {
    let executable = env::current_exe().expect("시험 프로그램 경로 확인");
    let grandchild = Command::new(executable)
        .arg("--grandchild")
        .spawn()
        .expect("제한된 손자 프로세스 생성");
    println!("시험용 손자 PID={}", grandchild.id());
    if let Some(path) = ready_path {
        std::fs::write(
            path,
            format!(
                "child={}\ngrandchild={}\n",
                std::process::id(),
                grandchild.id()
            ),
        )
        .expect("취소 시험 준비 파일 기록");
    }
    thread::sleep(SAFETY_TIMEOUT);
}

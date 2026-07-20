use std::env;
use std::process::Command;
use std::thread;
use std::time::Duration;

const SAFETY_TIMEOUT: Duration = Duration::from_secs(30);

fn main() {
    match env::args().nth(1).as_deref() {
        Some("--child") => child(),
        Some("--grandchild") => thread::sleep(SAFETY_TIMEOUT),
        _ => parent(),
    }
}

// 대표 프로세스가 먼저 끝난 뒤에도 제한된 자식 하나를 일부러 남긴다.
// TaskCage가 대표 PID만이 아니라 작업 cgroup 전체를 정리하는지 확인하기 위한 동작이다.
#[allow(clippy::zombie_processes)]
fn parent() {
    let executable = env::current_exe().expect("시험 프로그램 경로 확인");
    let child = Command::new(executable)
        .arg("--child")
        .spawn()
        .expect("제한된 자식 프로세스 생성");
    println!("시험용 자식 PID={}", child.id());
    thread::sleep(Duration::from_millis(200));
}

// 자식은 손자 하나만 만들고 최대 30초 뒤에는 스스로 끝난다.
#[allow(clippy::zombie_processes)]
fn child() {
    let executable = env::current_exe().expect("시험 프로그램 경로 확인");
    let grandchild = Command::new(executable)
        .arg("--grandchild")
        .spawn()
        .expect("제한된 손자 프로세스 생성");
    println!("시험용 손자 PID={}", grandchild.id());
    thread::sleep(SAFETY_TIMEOUT);
}

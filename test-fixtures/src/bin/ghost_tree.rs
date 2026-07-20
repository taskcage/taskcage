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

// 대표 프로세스가 먼저 끝난 뒤에도 제한된 자식 프로세스를 일부러 남긴다.
// TaskCage가 대표 PID만이 아니라 작업 cgroup 전체를 정리하는지 확인하기 위한 동작이다.
#[allow(clippy::zombie_processes)]
fn parent() {
    let executable = env::current_exe().expect("resolve ghost fixture executable");
    let child = Command::new(executable)
        .arg("--child")
        .spawn()
        .expect("spawn bounded ghost child");
    println!("spawned ghost child pid={}", child.id());
    thread::sleep(Duration::from_millis(200));
}

// 자식은 손자 프로세스를 하나 만들고, cgroup 전체 종료 신호가 올 때까지 둘 다 기다린다.
#[allow(clippy::zombie_processes)]
fn child() {
    let executable = env::current_exe().expect("resolve ghost fixture executable");
    let grandchild = Command::new(executable)
        .arg("--grandchild")
        .spawn()
        .expect("spawn bounded ghost grandchild");
    println!("spawned ghost grandchild pid={}", grandchild.id());
    thread::sleep(SAFETY_TIMEOUT);
}

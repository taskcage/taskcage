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

// This fixture intentionally abandons a bounded child so TaskCage can prove
// whole-cgroup cleanup after the leader exits.
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

// The child intentionally keeps a bounded descendant alive until cgroup.kill.
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

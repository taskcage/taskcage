use std::env;
use std::thread;
use std::time::Duration;

fn main() {
    let bytes = env::args()
        .nth(1)
        .expect("memory-hog requires a byte count")
        .parse::<usize>()
        .expect("memory-hog byte count must fit usize");
    let seconds = env::args()
        .nth(2)
        .expect("memory-hog requires a hold duration")
        .parse::<u64>()
        .expect("memory-hog hold duration must fit u64");

    let mut allocation = vec![0_u8; bytes];
    for page in allocation.chunks_mut(4096) {
        page[0] = 1;
    }
    thread::sleep(Duration::from_secs(seconds));
    std::hint::black_box(allocation);
}

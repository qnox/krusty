use std::io::Read;

fn main() {
    let mut byte = [0u8; 1];
    let _ = std::io::stdin().read(&mut byte);
}

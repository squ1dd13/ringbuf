use std::{io::Read, thread, time::Duration};

use ringbuf::HeapRb;

fn main() {
for i in 0..64 {
    let buf = HeapRb::<u8>::new(64);
    let (mut prod, mut cons) = buf.split();

    let smsg = "The quick brown fox jumps over the lazy dog".repeat(1024 * 1024);

    let msg = smsg.clone();
    let pjh = thread::spawn(move || {
        println!("-> sending message: '{}...'", &msg[..16]);

        let mut bytes = msg.as_bytes().chain(&[0][..]);
        loop {
            if prod.is_full() {
                // Spin lock
                //println!("-> buffer is full, waiting");
                //thread::sleep(Duration::from_millis(1));
            } else {
                let n = prod.read_from(&mut bytes, None).unwrap();
                if n == 0 {
                    break;
                }
                //println!("-> {} bytes sent", n);
            }
        }

        println!("-> message sent");
    });

    let cjh = thread::spawn(move || {
        println!("<- receiving message");

        let mut bytes = Vec::<u8>::new();
        loop {
            if cons.is_empty() {
                if bytes.ends_with(&[0]) {
                    break;
                } else {
                    // Spin lock
                    //println!("<- buffer is empty, waiting");
                    //thread::sleep(Duration::from_millis(1));
                }
            } else {
                let n = cons.write_into(&mut bytes, None).unwrap();
                //println!("<- {} bytes received", n);
            }
        }

        assert_eq!(bytes.pop().unwrap(), 0);
        let msg = String::from_utf8(bytes).unwrap();
        println!("<- message received: '{} ...'", &msg[..16]);

        msg
    });

    pjh.join().unwrap();
    let rmsg = cjh.join().unwrap();

    assert!(smsg == rmsg);
    println!("{} done", i);
}
}

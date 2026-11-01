use enigo::Key;
use pianomi::keycode::resolve_key;

fn main() {
    // White keys and their sharps from the authoritative Virtual Piano mapping.
    let cases = [
        '1', '!', '2', '@', '5', '%', '0', ')', 'q', 'Q', 't', 'T', 'y', 'Y', 'i', 'I', 'o', 'O',
        'p', 'P', 'a', 's', 'm',
    ];
    for &c in &cases {
        match resolve_key(&c.to_string()) {
            Some((k, shift)) => {
                if let Key::Other(vk) = k {
                    let base = if shift { "SHIFT+" } else { "" };
                    println!("char '{c}' -> {base}0x{vk:02X}  (shift={shift})");
                } else {
                    println!("char '{c}' -> {:?} (shift={shift})", k);
                }
            }
            None => println!("char '{c}' -> NONE (unmapped!)"),
        }
    }
}

use aricode_input::{Event, HidBootKeyboard, EV_KEY};
#[test]
fn every_new_key_in_a_full_report_is_delivered() {
    let mut keyboard = HidBootKeyboard::new();
    let mut out = [Event::default(); 16];
    keyboard.feed(&[0xff, 0, 4, 5, 6, 7, 8, 9], &mut out);
    let report = [0, 0, 10, 11, 12, 13, 14, 15];
    let n = keyboard.feed(&report, &mut out);
    let presses = out[..n].iter().filter(|e| e.kind == EV_KEY && e.value == 1).count();
    let repeated = keyboard.feed(&report, &mut out);
    println!("events={n}, new_presses={presses}, repeated_report_events={repeated}");
    assert_eq!(presses, 6, "las seis teclas nuevas deben llegar al consumidor");
}

fn main() {
    let caught = std::panic::catch_unwind(|| panic!("synthetic unwind probe"));
    assert!(caught.is_err());
    println!("UNWIND_OK");
}

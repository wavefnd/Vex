pub fn status(action: &str, message: impl AsRef<str>) {
    eprintln!("{action:>12} {}", message.as_ref());
}

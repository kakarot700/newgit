//! Keep newgit::VERSION honest against Cargo.toml.

#[test]
fn version_matches_cargo_toml() {
    let toml = include_str!("../Cargo.toml");
    let v = toml
        .lines()
        .find(|l| l.starts_with("version ="))
        .expect("Cargo.toml must declare version");
    let v = v.split('"').nth(1).unwrap();
    assert_eq!(v, newgit::VERSION);
}

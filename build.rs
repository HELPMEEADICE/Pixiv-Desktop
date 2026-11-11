fn main() {
    println!("cargo:rerun-if-changed=ui/");
    println!("cargo:rerun-if-changed=assets/");
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");
}

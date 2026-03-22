fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../dist");
    println!("cargo:rerun-if-changed=../src");
    println!("cargo:rerun-if-changed=../public");
    println!("cargo:rerun-if-changed=../index.html");
    println!("cargo:rerun-if-changed=../package.json");
    println!("cargo:rerun-if-changed=../vite.config.ts");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    tauri_build::build()
}

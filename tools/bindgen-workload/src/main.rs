fn main() {
    // The generated bindings are referenced so the build output is used.
    let _bindings = include_str!(concat!(env!("OUT_DIR"), "/bindings.rs"));
    println!("bindgen workload built");
}

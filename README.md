# rust-fuse3-template
A template for a Rust project using [fuse3](https://github.com/Sherlock-Holo/fuse3).

It has a basic implementation of a filesystem with a single file with basic methods for a fs and the wrapper FUSE implementation.

Implement `fs::Filesystem` for your fs.

# Run

```bash
cargo run -- -m <mount-point>
```

Where `<mount-point>` is the dir you want to mount the fs.

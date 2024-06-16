# rust-fuse3-template
A template for a Rust project using fuse3

It uses [fuse3](https://github.com/Sherlock-Holo/fuse3) and it has a very basic implementation of a filesystem for a single file with basic methods for a fs and the wrapper FUSE implementation.

# Run

```bash
cargo run -- -m <mount-point>
```

Where `<mount-point>` is the dir you want to mount the fs.

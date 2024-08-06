# rust-fuse3-template

A template for a Rust project using [fuse3](https://github.com/Sherlock-Holo/fuse3).

It has a basic implementation of a filesystem with a single file with basic methods for a fs and the wrapper FUSE
implementation.

# How to built from it

1. Implement `crate::fs::Filesystem` for your fs and change in `crate::mount::fuse3:Fuse3::new` (`src/mount/fuse3.rs:134`)
to use your implementation.
2. Replace `fuse3-template` and `fuse3_template` with your app name and package everywhere. **Safer is to do a text search in the whole project.**

# Run

```bash
cargo run -- -m <mount-point>
```

Where `<mount-point>` is the dir you want to mount the fs.

# Contribute

Feel free to fork it, change and use it in any way that you want.
If you build something interesting and feel like sharing pull requests are always appreciated.

## How to contribute

Please see [CONTRIBUTING.md](CONTRIBUTING.md).

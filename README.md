# Scalpel

Scalpel is yet another MD@Home implementation. However, this implementation is geared towards
performance and stability. Other available clients either have lots of overhead or aren't well
optimized, or both. Scalpel aims to work around underpowered systems that may bottleneck on other
clients.

## Instability Warning

Although the first paragraph claimed that Scalpel is meant to be a stable product, it is still in
the alpha stages of development. This means that there is *probably* implementation bugs that still
need to be weeded out, or features that are missing. It's in your best interest to skip this for
now and use something like [the official client](https://gitlab.com/mangadex-pub/mangadex_at_home/-/tree/master/docker)
or [lflare's Go client](https://github.com/lflare/mdathome-golang).

## Contributing

Submitting Issues and Pull Requests is absolutely encouraged. Issues may contain feature requests
and PRs may contain feature implementations. For both, please document the full intention of the
feature at hand. Please note: Not all requests may be accepted, and it is ultimately up the author
to accept or decline a feature request.

## Why "Scalpel"

For starters, there's already a [rust client](https://github.com/edward-shen/mangadex-home-rs) in
development, which would conflict in names. Plus, `MD@Home Rust` or `mdah-rs` sounds sort-of boring.
Scalpel was only chosen because "it sounded cool," there's no meaning behind it. Because of this,
the name of the project may change in the future.

## Support and Questions

Support is and answering questions is not guaranteed for this client. To be honest, if you don't
know how to use it, then you should probably stick to using the official client instead.

## Features

- Ultra Fast HTTP server
- Customizable TLS setup
- Battletested & Fast Cache Engine (with compression!1!)
- HTTP Gzip Support
- Pretty Console Logging
- Support for JSON & YAML configurations
- Docker Support / Builds

### Features-to-come

- Prometheus / VictoriaMetrics
- More Cache Engines (like plain filesystem or MongoDB)
- Non-hardcoded Configuration Path
- Better streaming of cache `MISS`es

## Getting Started

You can obtain the binary by building it youself or using a docker image. See
[Docker Support](#docker-support) if you would like to run your client with docker, otherwise see
the [Building](#building) section to see how to build the client on your platform. Thereafter, copy
the binary and the `settings.sample.yaml` (renamed to `settings.yaml`) to the directory that you
would like to run the client in.

Then, configure your client (hint: [Configuration](#configuration)) by editing `settings.yaml`,
and make sure to include your client secret, then start using:

- `./scalpel` on linux (via command line)
- Double clicking `scalpel.exe` on windows

### Docker Support

Scalpel can be downloaded and run using a docker image on platforms that support docker. To get started
using docker, see the [docker](https://github.com/blockba5her/scalpel/tree/main/docker) documentation.

### Building

Building the binary on all platforms is essentially similar, you just need to make sure you have
all of the required toolchain.

**Requirements for Building**

- Stable Rust Toolchain
- Clang compiler
- LLVM linker
- OpenSSL
- Patience

**Building For Linux**

1. Open terminal in directory with `Cargo.toml`
2. Run `cargo build --release`
3. Binary is located at `target/release/scalpel`

**Building For Windows**

1. Download Tooling:
    - Install [Git Bash](https://gitforwindows.org). Includes Clang and LLVM.
    - Install [Strawberry Perl](https://strawberryperl.com). Needed for building OpenSSL.
2. Open Git Bash terminal in directory with `Cargo.toml`
3. Run `PATH=/c/Strawberry/perl/bin:$PATH` (assuming you installed Strawberry Perl at
`C:\Strawberry`)
4. In the same terminal, run `cargo build --release`
5. Binary is located at `target/release/scalpel.exe`

## Configuration

All configuration options for the client can be found in `settings.sample.yaml`. All settings are
documented with what they do for the client.

The client will read configurations from `settings.yaml` or `settings.json` in the current working
directory, first looking for the `settings.yaml` configuration. All settings in the `settings.yaml`
file may be transposed into the `settings.json` file and will work exactly the same, however it's
recommended to keep the `yaml` configuration as it can be commented on.

### Log Level

Changing the log level is not in the client configuration. Instead, to change the log level you must
change the `RUST_LOG` environment variable before starting the client. The accepted levels are:

- `TRACE`
- `DEBUG`
- `INFO`
- `WARN`
- `ERROR`

The default and recommended client log level is `INFO`. For more info on changing log levels, see
[env_logger documentation](https://docs.rs/env_logger/0.8.3/env_logger/index.html). Example of
changing the log level on linux:
`RUST_LOG=DEBUG ./scalpel`

## Cache Engines

Cache Engines are the backbone of the client. They handle the storage and retrieval of images from
disk (or maybe somewhere else) for the HTTP client. These implementations must also conform to space
requirements mandated by the configuration file.

Each cache engine implementation can be selected by changing the `cache_engine` configuration option
to the one of the Engine Keys below. Most of the caches require additional settings.

### RocksDB

`cache_engine: rocksdb`

Currently the only cache engine, [RocksDB](https://rocksdb.org/) is a battle-tested key-value storage
system developed by Facebook. It's extremely fast and is used in many SQL databases like MySQL or
MariaDB, and includes a whole heap of customizability for different applications.

To configure, change all options under the `rocksdb_options` umbrella section in the configuration
file. See `settings.sample.yaml` for documentation on each option. If you don't know what an option
does, then you probably don't need to change it.

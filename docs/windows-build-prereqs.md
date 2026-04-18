# Windows Build Prerequisites

What you need on a fresh Windows machine to build `mediamount-agent` and the UFB installer from source. End users of the installed app need none of this — the bundled MSI handles the WinFsp runtime.

## Runtime vs. build dependencies

| | End user (installed app) | Developer (build from source) |
|---|---|---|
| WinFsp runtime (`winfsp-x64.dll`) | Installed by `ufb-tauri-setup-*.exe` via bundled MSI | Required |
| WinFsp Developer SDK (headers + import lib) | — | Required |
| LLVM / libclang | — | Required |
| Rust toolchain | — | Required (1.75+) |

## 1. Rust

Install via [rustup.rs](https://rustup.rs/). Add the MSVC target:

```powershell
rustup target add x86_64-pc-windows-msvc
```

MSVC Build Tools also required — install "Desktop development with C++" from the Visual Studio Installer.

## 2. WinFsp Developer SDK

The `winfsp-sys` crate needs WinFsp headers at `C:\Program Files (x86)\WinFsp\inc\` and the import lib at `C:\Program Files (x86)\WinFsp\lib\`. These come with the **Developer SDK**, not the runtime-only MSI.

Download the full WinFsp installer from <https://winfsp.dev/rel/>, run it, and check the "Developer" feature during install. The same installer bundles the runtime, so installing the SDK also installs the runtime.

Our bundled MSI (`installer/vendor/winfsp-2.1.25156.msi`) is the **runtime-only** build — it's enough for end users but doesn't include headers.

## 3. LLVM (for `winfsp-sys` bindgen)

`winfsp-sys` uses `bindgen` at build time to regenerate bindings against the installed WinFsp headers. `bindgen` needs `libclang`.

Install LLVM via one of:

- [LLVM releases](https://github.com/llvm/llvm-project/releases) — pick `LLVM-*-win64.exe`, enable "Add LLVM to PATH"
- `winget install LLVM.LLVM`
- `choco install llvm`

Verify: `where libclang` should return a path. If `bindgen` still fails, set `LIBCLANG_PATH=C:\Program Files\LLVM\bin`.

## 4. Inno Setup (for installer packaging)

Only needed if you're building `ufb-tauri-setup-*.exe`:

- Inno Setup 6 — <https://jrsoftware.org/isdl.php>
- The compiler is at `C:\Program Files (x86)\Inno Setup 6\ISCC.exe`

## Verifying the build

```powershell
cd mediamount-agent
cargo build --target x86_64-pc-windows-msvc
```

Agent binary lands at `mediamount-agent\target\x86_64-pc-windows-msvc\debug\mediamount-agent.exe`.

## CI notes

GitHub Actions `windows-latest` runners ship with MSVC tools + Rust. They also include Chocolatey, so LLVM + WinFsp SDK can be installed with:

```yaml
- name: Install WinFsp SDK + LLVM
  run: |
    choco install winfsp -y --version=2.1.0.25156
    choco install llvm -y
```

Then `cargo build` as normal. The CI doesn't need Inno Setup unless packaging releases.

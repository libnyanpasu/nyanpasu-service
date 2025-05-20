
## [1.3.0] - 2025-05-20


### ✨ Features

- Support SAFE_PATHS by @greenhat616 


### 🐛 Bug Fixes

- Ci by @greenhat616 

- Provide service stop pending ttl by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.2.0...v1.3.0




## [1.2.0] - 2025-02-23


### ✨ Features

- **ws:** Support log notify by @greenhat616 

- **ws:** Support core state changed notify by @greenhat616 

- Static crt for windows by @greenhat616 

- Ws events layer and refactor core manager handle to owned by app state by @greenhat616 

- Support win service gracefully shutdown by @greenhat616 

- Specta support by @greenhat616 


### 🐛 Bug Fixes

- Linter by @greenhat616 


### 🔨 Refactor

- Use deno script to get version by @greenhat616 

- Use axum listener trait and axum gracefully shutdown by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.1.3...v1.2.0




## [1.1.3] - 2025-01-09


### 🐛 Bug Fixes

- **server:** Use merge instead of nest for axum by @greenhat616 

- Clippy by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.1.2...v1.1.3




## [1.1.2] - 2025-01-09


### 🐛 Bug Fixes

- **macos:** Support set dns by @greenhat616 

- Lint by @greenhat616 

- Correct exit code usage by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.1.1...v1.1.2




## [1.1.1] - 2025-01-08


### 🐛 Bug Fixes

- Do not replace binary if src and dst are same by @greenhat616 

- Use /usr/bin on linux by @greenhat616 

- Bump axum to 0.8 by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.1.0...v1.1.1




## [1.1.0] - 2024-12-27


### 🐛 Bug Fixes

- **ci:** Try to fix release ci by @greenhat616 

- **lifecycle:** Make service exit gracefully by @greenhat616 

- **macos:** Fix a status check condition by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.0.7...v1.1.0




## [1.0.7] - 2024-10-09


### ✨ Features

- Support clash rs alpha by @greenhat616 


### 🐛 Bug Fixes

- Handle service shutdown signal by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.0.6...v1.0.7




## [1.0.6] - 2024-09-10


### 🐛 Bug Fixes

- Bump simd-json to fix x86 build by @greenhat616 

-----------------



**Full Changelog**: https://github.com/libnyanpasu/nyanpasu-service/compare/v1.0.5...v1.0.6



## [1.0.0] - 2024-07-24

### ✨ Features

- Better version print by @greenhat616 

- Add a logs cmd to get server logs by @greenhat616 

- Add core rpc calls by @greenhat616 

- Add slef update command by @greenhat616 

- Cleanup socket file and cleanup zombie instance before startup by @greenhat616 

- Add deadlock detection and status skip-service-check flag by @greenhat616 

- Add status client rpc check by @greenhat616 

- Add acl for server by @greenhat616 

- Core restart & stop rpc api by @greenhat616 

- Core start rpc api by @greenhat616 

- Service server startup and status inspect rpc by @greenhat616 

- Status command by @greenhat616 

- Restart command by @greenhat616 

- Stop command by @greenhat616 

- Stop command by @greenhat616 

- Start command by @greenhat616 

- Unstall command by @greenhat616 

- Install command by @greenhat616 

- Add core manager util by @greenhat616 

- Draft http client ipc by @greenhat616 

- Draft server ipc by @greenhat616 

- Add config file by @zzzgydi 

- Update by @zzzgydi 

- Add install and uninstall bin by @zzzgydi 


### 🐛 Bug Fixes

- Publish ci by @greenhat616 

- Lint by @greenhat616 

- Macos user ops by @greenhat616 

- Lint by @greenhat616 

- Issues by @greenhat616 

- Ci by @greenhat616 

- Lint by @greenhat616 

- Ci by @greenhat616 

- Ci by @greenhat616 

- Refresh process table before kill process by @greenhat616 

- Check pid whether is exist before killing zombie server by @greenhat616 

- Publish version ctx by @greenhat616 

- Lint by @greenhat616 

- Rpc inspect logs by @greenhat616 

- Process service stop signal by @greenhat616 

- Missing PathBuf mod import by @keiko233 

- Lint by @greenhat616 

- Socket file permission is not changed by @greenhat616 

- Mark socket not execuable by @greenhat616 

- Lint by @greenhat616 

- Lint by @greenhat616 

- Mark unix socket group rw able by @greenhat616 

- State match by @greenhat616 

- Lint by @greenhat616 

- The status query for launchd by @greenhat616 

- Setup windows service manager by @greenhat616 

- Setup windows service manager by @greenhat616 

- Correct macOS group creation command in create_nyanpasu_group function by @keiko233 

- Logging guard is dropped too early by @greenhat616 

- Service manager encoding issue by @greenhat616 

- Lint by @greenhat616 

- Missing use by @greenhat616 

- Issue by @greenhat616 

- Correct server args by @greenhat616 

- Error handling in `check_and_create_nyanpasu_group` function by @keiko233 

- Correct return type for is_nyanpasu_group_exists function by @keiko233 

- Upstream status check by @greenhat616 

- Update dependencies by @greenhat616 

- Rename meta to mihomo and support clash-rs, mihomo-alpha by @greenhat616 


### 📚 Documentation

- Update readme by @greenhat616 


### 🧹 Miscellaneous Tasks

- Bump crates by @greenhat616 

- Apply linting fixes with rustfmt by @github-actions[bot] 

- Use tracing-panic to better capture panic info by @greenhat616 

- Add debug info for os operations by @greenhat616 

- Cleanup deps by @greenhat616 

- Enable tokio-console for debug by @greenhat616 

- Version info use table output by @greenhat616 

- Add editorconfig by @greenhat616 

- Fmt by @greenhat616 

- Add debug print by @greenhat616 

- Apply linting fixes with rustfmt by @github-actions[bot] 

- Add stop advice by @greenhat616 

- Update actions/checkout action to v4 (#24) by @renovate[bot]  in [#24](https://github.com/LibNyanpasu/nyanpasu-service/pull/24)

- Apply linting fixes with rustfmt by @github-actions[bot] 

- Apply linting fixes with rustfmt by @github-actions[bot] 

- Apply linting fixes with clippy by @github-actions[bot] 

- Apply linting fixes with rustfmt by @github-actions[bot] 

- Apply linting fixes with clippy by @github-actions[bot] 

- Commit workspace by @greenhat616 

- Draft api ctx definition by @greenhat616 

- Rename --debug to --verbose by @greenhat616 

- Commit workspace by @greenhat616 

- Commit workspace by @greenhat616 

- Update actions/checkout action to v4 (#3) by @renovate[bot]  in [#3](https://github.com/LibNyanpasu/nyanpasu-service/pull/3)

- Add renovate.json (#2) by @renovate[bot]  in [#2](https://github.com/LibNyanpasu/nyanpasu-service/pull/2)

- Code format by @zzzgydi 

- Ci by @zzzgydi 

- Init by @zzzgydi 

-----------------



## New Contributors
* @github-actions[bot] made their first contribution
* @keiko233 made their first contribution
* @renovate[bot] made their first contribution in [#24](https://github.com/LibNyanpasu/nyanpasu-service/pull/24)
* @zzzgydi made their first contribution



## [1.0.1] - 2024-07-24

### 🐛 Bug Fixes

- Replace dscl to dseditgroup by @keiko233 

- Update rust crate clap to v4.5.10 (#29) by @renovate[bot]  in [#29](https://github.com/LibNyanpasu/nyanpasu-service/pull/29)


### 🧹 Miscellaneous Tasks

- Up by @greenhat616 

- Update rust crate tokio to v1.39.1 (#30) by @renovate[bot]  in [#30](https://github.com/LibNyanpasu/nyanpasu-service/pull/30)

-----------------



**Full Changelog**: https://github.com/LibNyanpasu/nyanpasu-service/compare/v1.0.0...v1.0.1



## [1.0.2] - 2024-07-26

### 🐛 Bug Fixes

- Update rust crate interprocess to 2.2.1 by @renovate[bot]  in [#34](https://github.com/LibNyanpasu/nyanpasu-service/pull/34)


### 🧹 Miscellaneous Tasks

- Update rust crate parking_lot to 0.12.3 by @renovate[bot]  in [#33](https://github.com/LibNyanpasu/nyanpasu-service/pull/33)

- Update rust crate clap to 4.5.10 by @renovate[bot]  in [#32](https://github.com/LibNyanpasu/nyanpasu-service/pull/32)

- Update rust crate axum to 0.7.5 by @renovate[bot]  in [#31](https://github.com/LibNyanpasu/nyanpasu-service/pull/31)

-----------------



**Full Changelog**: https://github.com/LibNyanpasu/nyanpasu-service/compare/v1.0.1...v1.0.2



## [1.0.3] - 2024-07-26

### ✨ Features

- Support sidecar path search and share the status type with ui by @greenhat616 

-----------------



**Full Changelog**: https://github.com/LibNyanpasu/nyanpasu-service/compare/v1.0.2...v1.0.3



## [1.0.4] - 2024-07-28

### 🐛 Bug Fixes

- Should start service after updated by @greenhat616 


### 🔨 Refactor

- Use atomic cell to hold flag and state, and add a recover core logic by @greenhat616 


### 🧹 Miscellaneous Tasks

- Sync latest nyanpasu-utils by @greenhat616 

-----------------



**Full Changelog**: https://github.com/LibNyanpasu/nyanpasu-service/compare/v1.0.3...v1.0.4



## [1.0.5] - 2024-07-28

### 🐛 Bug Fixes

- Fetch status deadlock by @greenhat616 

- Up by @greenhat616 


### 🧹 Miscellaneous Tasks

- Add a error log for deadlock debug use by @greenhat616 

- Add a timeout seq for status by @greenhat616 

- Update rust crate clap to 4.5.11 by @renovate[bot]  in [#35](https://github.com/LibNyanpasu/nyanpasu-service/pull/35)

- Apply linting fixes with rustfmt by @github-actions[bot] 

- Mark start req as Cow by @greenhat616 

-----------------



**Full Changelog**: https://github.com/LibNyanpasu/nyanpasu-service/compare/v1.0.4...v1.0.5




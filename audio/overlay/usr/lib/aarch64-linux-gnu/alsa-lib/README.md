# overlay/usr/lib/aarch64-linux-gnu/alsa-lib/README.md

Inferno 的 ALSA 插件 `libasound_module_pcm_inferno.so` 由 `build-inferno.sh` 产出并在此目录注入。
源码树不提交二进制；`install-into-image.sh --release release` 会从 `release/lib/` 复制过来。

# zidoo-fixed48-test — AudioTrack 采样率 vs 芝杜光纤输出测试 APK

## 这是什么

芝杜 Z9X-8K 的光纤（TOSLink/SPDIF）输出速率是否由第三方 App 的 AudioTrack 采样率决定？
这个最小 APK 就是用来验证该前提的：

- 按钮 **Play 48 kHz tone** / **Play 44.1 kHz tone**：用 AudioTrack（MODE_STREAM、16-bit PCM、
  立体声）循环写 1 kHz 正弦波 10 s（可切换 10/30/60 s），并显示 getSampleRate()、
  getFormat()、AudioManager PROPERTY_OUTPUT_SAMPLE_RATE、getPlaybackParams() 实际值；
- 按钮 **Play file (WAV ...)**：优先从 MediaStore，其次从 /sdcard/Music、Download 找 .wav，
  自解析 RIFF（PCM 8/16/24/32-bit 或 float32）转 16-bit 立体声后用 WAV 自身采样率播放。
  芝杜上现成可用的文件：/sdcard/Music/khaen-toslink-48k-24s.wav；
- 每次播放追加一行 JSON 到 App 私有目录
  /storage/emulated/0/Android/data/com.khaen.fixed48test/files/fixed48test-results.jsonl
  （可通过 SMB 直接读，无需 adb）。

包名 com.khaen.fixed48test，minSdk/targetSdk 30，纯 Java、无 native、无 AndroidX，
armeabi-v7a userspace 设备可直接运行。

## 构建（手工 aapt2 + javac + d8 + apksigner，无 gradle）

    powershell -NoProfile -ExecutionPolicy Bypass -File build.ps1

产物：artifacts/khaen-audio/zidoo-test/zidoo-fixed48-test-v1.0.apk + SHA256SUMS.txt；
构建证据写到 rt-stack/evidence/calib-chain-20261121/zidoo-apk48/build-record/。

- 工具链：Android SDK build-tools/35.0.0（注意：34.0.0 的 d8 对 JDK21 编出的 class 会内部
  NPE，必须用 35.0.0）、platforms/android-35/android.jar、JDK 21（javac -source 8 -target 8）。
- 签名 keystore：首次运行自动生成在 scratch/apk48/keystore/zidoo-fixed48.jks
  （scratch/ 已被 .gitignore 忽略；这是丢弃型测试密钥，不提交仓库）。
- 重新构建需要复用同一 keystore 才能覆盖安装。

## 安装到芝杜

先拷贝到 SMB 共享（不改系统、不 root、不 adb）：

    powershell -NoProfile -ExecutionPolicy Bypass -File stage.ps1
    powershell -NoProfile -ExecutionPolicy Bypass -File stage.ps1 -OpenInstaller

然后在芝杜屏幕上确认 Install（安装动作必须由用户在设备上点击）。

## 验证协议

见 rt-stack/evidence/calib-chain-20261121/zidoo-apk48/PROTOCOL.md：
停 host -> MOTU 光口 48k/Internal -> arecord 抓 20ch -> 分别点 48k / 44.1k 按钮 ->
用 analyze_capture.py 看 ch12/13 是否有干净 1 kHz、MOTU 是否锁定。

analyze_capture.py 也可单独复用：

    python analyze_capture.py cap.wav --channels 12,13

## 结论（当前状态）

- APK 已构建并通过静态校验（apksigner verify PASS、aapt2 badging、WAV 解析器与 Python 参考实现
  逐样本对拍一致）。
- 设备侧验证 NOT_RUN：本会话未安装、未抓 MOTU、未下结论。
- 结果分支与判据见 PROTOCOL.md 第 4 节。
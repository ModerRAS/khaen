# /etc/camilladsp/coeffs

放 FIR/IIR 系数文件（由用户/校准流程提供，OEM 镜像不预置业务系数）：
- WAV：单声道 32-bit float，采样率与处理采样率一致（48k 或 96k）；CamillaDSP `Conv type: Wav` 读取。
- RAW 文本：每行一个系数（CamillaDSP `Conv type: Raw`，默认 TEXT 格式）。

示例：把左右声道各自系数放到 `fir_l.wav` / `fir_r.wav`，然后在 /etc/camilladsp/dante.yml 的
`filters` 段落加 Conv 过滤器并在 pipeline 中引用（见 audio/configs/fir/ 示例）。
注意：FIR 长度/采样率与模式绑定——48k 与 96k 需要各自生成，不能混用。

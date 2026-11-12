# 启动本地语音识别服务（whisper.cpp 的 whisper-server）
#
# 用法：
#   pnpm stt
#   pnpm stt:gpu                                       # 用 NVIDIA 显卡跑（要装过 GPU 版）
#   powershell -ExecutionPolicy Bypass -File scripts/stt-server.ps1 -Port 28081
#   powershell -ExecutionPolicy Bypass -File scripts/stt-server.ps1 -Backend cuda
#   powershell -ExecutionPolicy Bypass -File scripts/stt-server.ps1 -Model C:\path\ggml-medium.bin
#
# 前置：~/.shania/tools/whisper/Release/whisper-server.exe            （CPU 版）
#       ~/.shania/tools/whisper-cuda/Release/whisper-server.exe       （GPU 版）
#       ~/.shania/models/ggml-small-q5_1.bin
#
# 桌面端会在设置里自动装到同样的目录，这个脚本只是方便命令行调试。

param(
    [int]$Port = 28080,
    [ValidateSet("cpu", "cuda")]
    [string]$Backend = "cpu",
    [string]$Model = "",
    [int]$Threads = 0,
    [string]$Language = "auto"
)

$ErrorActionPreference = "Stop"
$root = Join-Path $HOME ".shania"
$dirName = if ($Backend -eq "cuda") { "whisper-cuda" } else { "whisper" }
$serverExe = Join-Path $root "tools\$dirName\Release\whisper-server.exe"
$ffmpegExe = Join-Path $root "tools\ffmpeg\bin\ffmpeg.exe"

if (-not $Model) {
    $Model = Join-Path $root "models\ggml-small-q5_1.bin"
}

if (-not (Test-Path $serverExe)) {
    Write-Error "找不到 whisper-server：$serverExe（$Backend 版没装？桌面端设置 → 语音输入里可以切）"
    exit 1
}
if (-not (Test-Path $Model)) {
    Write-Error "找不到模型文件：$Model"
    exit 1
}
# --convert 会实际调用 ffmpeg 把 webm/opus 转成 16k 单声道 wav，缺了它接口会返回 400
if (-not (Test-Path $ffmpegExe)) {
    Write-Host "警告：没有 ffmpeg（$ffmpegExe），webm 录音将无法解码" -ForegroundColor Yellow
} else {
    $env:PATH = "$(Split-Path $ffmpegExe);$env:PATH"
}

# 默认按物理核数开线程：超线程数反而更慢
if ($Threads -le 0) {
    $Threads = (Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfCores -Sum).Sum
}

$modelSize = [math]::Round((Get-Item $Model).Length / 1MB)
Write-Host "模型：$(Split-Path $Model -Leaf)（${modelSize} MB），后端：$Backend" -ForegroundColor Cyan
Write-Host "监听：http://127.0.0.1:$Port/v1/audio/transcriptions（线程 $Threads）" -ForegroundColor Cyan
Write-Host "按 Ctrl+C 停止" -ForegroundColor DarkGray

# CPU 上 `-ac` 把音频上下文从 30 秒缩到 9 秒，短句快 2.5 倍；GPU 不需要
$extra = @()
if ($Backend -eq "cpu") { $extra += @("-ac", "450") }

# 注意：--inference-path 必须带前导斜杠，且不能被 shell 做路径转换
& $serverExe `
    --model $Model `
    --host 127.0.0.1 `
    --port $Port `
    --inference-path "/v1/audio/transcriptions" `
    --convert `
    --language $Language `
    --threads $Threads `
    @extra

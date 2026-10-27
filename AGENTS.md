# GabTwin · 协作约定

## 模型权重

- **不要自动下载模型/权重**（HF、hf-mirror、Ollama、ModelScope 等都不下）。
- 需要新模型时，只向用户提供：
  1. 仓库 id（如 `mlx-community/Qwen3.8-27B-4bit`）；
  2. 需要的确切文件（safetensors/gguf/tokenizer/config）；
  3. 总体积；
  4. 目标目录（如 `models/llm/<name>`）。
- 由用户自己下载；下载完成后我再做校验、接入、测试和切换。
- 替换默认模型前，先说明内存/速度/许可的取舍。

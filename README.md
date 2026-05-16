# pdf-splitter — PDF 批量切割工具

将大型 PDF 文件按大小或页数拆分为多个较小的 PDF 文件。提供图形界面（GUI）和命令行（CLI）两种操作方式。

## 功能

- **两种切割模式**
  - **按大小**：将 PDF 按文件大小切分，每块不超过指定上限（默认 50 MB）
  - **按页数**：将 PDF 按固定页数切分（默认每块 100 页）
- **批量处理**：支持单个文件、多个文件、或整个目录（递归）
- **智能分析**：自动识别电子版/扫描版 PDF，显示每份文件的页数、大小、分类和字数
- **提取图片**：GUI 模式下勾选文件后点击「提取图片」，将 PDF 内嵌图片按原始格式导出到输出目录。支持 /DCTDecode (.jpg)、/JPXDecode (.jp2)、/CCITTFaxDecode (.tif)；同一图片跨页共享时自动去重，输出文件按 `文件名_001.jpg` 格式编号。
- **GUI 模式**：基于 egui 的桌面图形界面，支持文件列表、选中/取消、进度显示
- **CLI 模式**：适合脚本集成，支持静默模式、进度条、切割后删除源文件
- **暂停/恢复/停止**：GUI 模式下可随时控制切割任务
- **性能优化**：通过前缀和累积测量和二分查找验证，避免 O(N²) 序列化开销

## 安装

### 从源码构建

```bash
# 调试构建
cargo build

# 发布构建
cargo build --release
```

发布构建位于 `target/release/pdf-splitter.exe`。

## 使用

### GUI 模式

直接运行程序（不带参数）：

```bash
pdf-splitter
```

在 GUI 中可以：

1. 点击「选择 PDF」或「选择目录」添加文件
2. 在文件列表中勾选需要切割的文件
3. 选择切割方式（按大小/按页数）并设置参数
4. 点击「开始切割」开始切割，或勾选文件后点击「提取图片」导出图片

### CLI 模式

```bash
# 按大小切割（默认 50 MB）
pdf-splitter input.pdf

# 按大小切割，每块上限 100 MB
pdf-splitter --max-size 100 input.pdf

# 按页数切割，每块 50 页
pdf-splitter --mode pages --page-count 50 input.pdf

# 批量处理多个文件
pdf-splitter file1.pdf file2.pdf file3.pdf

# 处理整个目录（递归）
pdf-splitter ./pdfs/

# 切割后删除源文件
pdf-splitter --delete input.pdf

# 静默模式（无进度条）
pdf-splitter --quiet input.pdf
```

完整参数：

| 参数 | 说明 |
| ------ | ------ |
| `paths` | PDF 文件或目录路径（支持多个），省略时启动 GUI |
| `-s, --max-size` | 单块最大大小，单位 MB（默认 50） |
| `-p, --page-count` | 按页数切割时每块页数（默认 100） |
| `--mode` | 切割模式：`size`（默认）或 `pages` |
| `-d, --delete` | 切割成功后删除源文件 |
| `-q, --quiet` | 静默模式 |

## 输出

切割后的文件命名格式为 `原文件名_part1.pdf`、`原文件名_part2.pdf`、…… 输出到源文件所在目录（或指定的输出目录）。

## 构建

项目提供 `Makefile`（使用 PowerShell）：

```bash
make check       # 检查编译
make build       # 调试构建
make release     # 发布构建
make run         # 启动 GUI
make run-cli ARGS='--help'  # CLI 模式
make clean       # 清理构建缓存
```

## 技术细节

- 使用 [lopdf](https://github.com/J-F-Liu/lopdf) 库操作 PDF
- 按大小切割时采用前缀和累积测量 + 二分查找验证，保证每块大小严格不超过上限
- 按页数切割时使用渐进式克隆策略，避免重复克隆完整文档的 O(N²) 开销
- 支持 Unicode/中文文件名，GUI 中自动加载系统中文字体
- 后台分析使用 Rayon 并行池加速

## 许可证

MIT

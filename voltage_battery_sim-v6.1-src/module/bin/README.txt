本目录放置编译产物 bin/vbat_sim（aarch64-unknown-linux-musl 全静态单二进制）。

在 Linux/macOS 或 GitHub Actions 上执行：

  ./build.sh     # 交叉编译 → target/aarch64-unknown-linux-musl/release/vbat_sim
  ./package.sh   # 自动把二进制放入 module/bin/ 并打包成可刷入 zip

或直接触发 .github/workflows/build.yml（打 v* 标签 / 手动 dispatch），
产物 zip 即为含二进制的可刷入模块包。

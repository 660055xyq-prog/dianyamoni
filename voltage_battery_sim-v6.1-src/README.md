# 电压电量模拟服务 v6.0 (Rust 重构)

按 `V5 52 Rust Refactor Plan.md` 将 voltage_battery_sim_module v5.52 KSU 模块重构为
**v6.0 Rust**：`aarch64-unknown-linux-musl` 全静态单二进制，零动态库依赖。

## 目录结构

```
vbat_sim_v6/
├── Cargo.toml               # 仅 libc="0.2"，release 全静态优化
├── src/                     # Rust 源码（双角色单二进制）
│   ├── main.rs              # 24 个 CLI 子命令 + daemon 入口
│   ├── config.rs            # config.conf / learned.conf 读写 + 参数联动
│   ├── sysfs.rs             # sysfs 节点读取 / 电池节点探测 / 背光探测
│   ├── daemon.rs            # 守护进程结构体 + epoll 事件循环 + 主流水线
│   ├── ipc.rs               # AF_UNIX socket（CLI ↔ 守护进程）
│   ├── telemetry.rs         # 事件/退出原因/运行时状态/历史曲线
│   ├── platform/
│   │   ├── android.rs       # 通知/弹幕/dumpsys/svc/getprop
│   │   ├── uevent.rs        # NETLINK_KOBJECT_UEVENT（power_supply）
│   │   └── evdev.rs         # 音量键节点探测 + 盲操续命按键读取
│   └── algo/                # 算法（1:1 复刻 v5.52）
│       ├── filter.rs        # 双轨 3 点滑动中位数滤波
│       ├── soc.rs           # 三模式电量（线性/锂电/专属曲线）
│       ├── ir.rs            # 内阻学习 / IR 补偿 / 动态红线
│       ├── blackbox.rs      # 黑匣子写盘 + 开机复盘
│       ├── full_learn.rs    # 满电学习 / 静置满电压自学习
│       ├── calib.rs         # 断点引导校准（分箱中位数 + PAVA + WLS）
│       └── shutdown.rs      # 低电提醒 / 关机倒计时状态机
├── module/                  # KSU 模块引导壳（可刷入内容）
│   ├── module.prop          # v6.0
│   ├── customize.sh         # 安装迁移（9 数据文件）+ 权限
│   ├── service.sh           # 开机启动 + 看门狗自愈
│   ├── action.sh / config_helper.sh / uninstall.sh / config.conf
│   ├── bin/                 # vbat_sim 编译产物
│   └── webroot/             # 琪露诺主题 WebUI（原样保留）
├── .github/workflows/build.yml   # GitHub Actions 交叉编译 + 打包
├── build.sh                 # 本地交叉编译
└── package.sh               # 打包可刷入 zip
```

## 编译与打包

需要 `zig` + `cargo-zigbuild`（交叉编译 aarch64 musl 静态二进制）。

```bash
./build.sh          # 编 target/aarch64-unknown-linux-musl/release/vbat_sim
./package.sh        # 产出 dist/voltage_battery_sim-v6.0-rust.zip
```

或在 GitHub Actions：打 `v*` 标签或手动触发 workflow，产出产物与 Release。

## 双角色单二进制

```
vbat_sim daemon           # 守护进程（epoll + timerfd + uevent + unix socket + evdev）
vbat_sim <subcommand>     # 命令中心（status/set/cycle/calib_*/snooze/... 共 24 项）
```

守护在线时 CLI 通过 `$MODDIR/.vbat.sock` 热更/唤醒；停服降级为本地读盘。

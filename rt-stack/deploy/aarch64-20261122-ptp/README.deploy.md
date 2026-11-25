# TSPI PTPv2 grandmaster add-on bundle (aarch64-20261122-ptp

> 由 deploy/bin/make-ptp-bundle.sh 生成；安装/验证/回滚见同目录 INSTALL-ROLLBACK.md 与
> rt-stack/ptp/README.md、docs/BOARD-PTP-MASTER.md。板端 install/start = NOT_RUN。

- target: aarch64-unknown-linux-gnu（板端 Debian 12 / glibc >= 2.34）
- 内容: bin/{ptp4l,phc2sys,pmc,phc_ctl}、lib/（启动包装）、units/、etc/（配置模板）
- 安装（默认 dry-run；live 需授权窗口 scope=install-ptp-master）:
  `sudo bash <repo>/rt-stack/deploy/bin/09-install-ptp-gm.sh --live --authorize WINDOW.json --bundle <本目录>`
- 回滚: `sudo bash <repo>/rt-stack/deploy/bin/10-rollback-ptp-gm.sh --live --authorize WINDOW.json [--remove]`

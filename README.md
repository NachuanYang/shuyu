# 书隅 Shuyu

书隅是一款面向个人阅读场景的轻量级桌面阅读器。

## 目录

- app/：Vue与Tauri应用源码、数据库迁移、测试及构建配置。
- app/src/：阅读器界面与前端业务逻辑。
- app/src-tauri/：桌面端能力、数据存储与本地服务。

## 开发

需要Node.js/npm与Rust工具链。

进入app目录后运行：

    npm install
    npm run dev

启动桌面应用：

    npm run tauri -- dev

构建前端与桌面安装包：

    npm run build
    npm run tauri -- build

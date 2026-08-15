# 参考实现

tapecpy 的部分设备行为和功能需求参考了
[LTFSCopyGUI](https://github.com/zhaoyangwx/LTFSCopyGUI)。上游仓库目前没有声明允许
再分发的开源许可证，因此其源码不包含在本仓库中，也不属于 tapecpy 的 Apache-2.0
许可范围。

如需在本地对照实现，可自行获取：

```bash
git clone https://github.com/zhaoyangwx/LTFSCopyGUI.git references/LTFSCopyGUI
```

`references/LTFSCopyGUI/` 已被 Git 忽略。该源码仅用于研究设备命令、数据格式和可观察
行为；tapecpy 使用独立的 Linux 架构重新实现所需能力，不应直接复制或再分发上游源码。

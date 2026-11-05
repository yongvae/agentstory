# agentstory（命令 `ags`）

[English](README.md) · 中文

**一个用 Rust 写的、非常轻量的 Agent 记忆检索工具。** 只有一个 5 MB 的二进制文件，能把本机所有 AI 编程助手（Claude Code、Codex、Cursor、Gemini、Qwen、OpenCode、Hermes、Pi、DeepSeek Harness）的历史对话建成索引。搜一次约 30 毫秒，内存约 15 MB，还能让一个 Agent 接手另一个 Agent 的活。另外附带一套所有 Agent 共用的记忆、Wiki 和错题本。

## 实测指标

测试数据是本机真实的 Agent 历史，共 1.45 GB（339 个会话、6595 轮对话），机器是 Apple M3 Max。

| 指标 | 数值 |
|---|---|
| 程序体积 | **5.1 MB**，单个文件，SQLite 已内置，不需要装任何运行环境 |
| 检索（从启动进程到出结果，含中文） | 平均约 **32 毫秒**，内存峰值 **15 MB**，三档搜索都差不多 |
| 生成交接单 | 约 **20 毫秒**，21 MB |
| 没有新内容时的增量刷新 | **0.11 秒**，10 MB |
| 第一次全量建索引 | 13.3 秒，内存峰值 112 MB |
| 索引占用磁盘 | 145 MB，是原始记录的 10%；不存工具输出时 112 MB |
| 作为 MCP 服务空闲时 | 8.6 MB |

想自己复现，运行 `scripts/bench.sh`。它会用一个临时索引，不影响你正在用的那份。

**为什么这么轻**：
- 没有常驻进程，不调大模型，不用向量库。每条命令跑完就退出。
- 用 SQLite FTS5 的 trigram 分词，中文和词的一部分都能直接搜，不用装分词器，也不用下载模型。
- 增量更新：每次只检查文件有没有变，变了才重读。
- 只存要点：工具输出只保留开头 1500 字和结尾 500 字（报错一般在结尾），密钥会去掉，会话存成压缩过的统一格式，所以索引只有原始记录的约 10%。

## 它解决什么问题

同时用好几个 Agent（Claude Code、Codex、Cursor、Gemini……），或者在群里挂了好几个机器人，很快就会碰到下面这些事：

| 痛点 | 以前 | 用 agentstory |
|---|---|---|
| **换个 Agent 就失忆** | A 做到一半额度用完，B 接手时只能翻群聊、猜做到哪了 | `ags handoff <id>` 一秒出交接单：目标、后来补充的要求、停在哪、改过的文件、已经做过的提交、推送、代码评审、发出的消息、写过的文档和任务、定时任务、出过的错 |
| **「上周那个事是怎么定的？」** | 各家的记录格式不一样，存的位置也不一样，只能一个个翻 | `ags search 关键词`，一次搜遍所有 Agent 的历史，中文直接能搜 |
| **同一个错犯两遍** | 被纠正过的事没人记，下个会话照犯 | `ags corrections` 从历史对话里挑出你纠正过 Agent 的地方，写进错题本；所有 Agent 开工前都会读错题本 |
| **记忆越堆越乱、过时了也没人发现** | 一个大文件什么都往里写，新旧说法同时存在 | 一事一文件，过时的标成「被替代」而不是删掉；`ags doctor` 检查字数上限、坏链接和泄露的密钥 |
| **每个项目都要重新交代一遍规矩** | Claude 读 CLAUDE.md，Gemini 读 GEMINI.md，Cursor 读自己的规则，各写各的 | `ags init` 一条命令搭好空间，各家 Agent 都指向同一份 AGENTS.md |

## 五个真实案例

下面是作者自己的使用场景：几个聊天机器人（分别跑在 Claude Code 和 Cursor 上）共同服务一个人。数字都是在本机实测的。

### 1. 机器人额度用完，另一个接手

**以前**：主力机器人额度用完，备用机器人收到「请接手本会话」，结果话题里只有这一句。它只好去翻群聊，自己拼出任务是什么。

**现在**：

```sh
ags sessions --since today          # 找到刚才那个会话
ags handoff 3947cf05                # 出交接单
```

交接单里有：最初的目标、用户后来补充的每一句话、最后一轮停在哪、改过哪些文件、做过的提交、推送、代码评审、发出的消息、写过的文档和任务、定时任务（**这些不能重做**）、出过的错，以及「接手前先核对现状」的提醒。整张单子都是机械抽取的，不经过大模型，所以不会编造进度。

### 2. 跨 Agent 翻旧账

```sh
ags search 创作者中心                 # 找到 3 天前另一个机器人修的那个问题
ags show 398a58fe --around 创作者中心  # 跳到那段对话，前后各看两轮
```

本机实测：334 个会话、约 6500 轮对话（Claude Code、Cursor、Codex、Hermes 四家）。第一次全量建索引 7 秒，之后每次只读改过的文件，约 0.2 秒；搜一次约 0.1 秒。

### 3. 把纠正变成错题

用户说过一句「再深度思考一下，不要仅限于新闻」，当时没人记下来。`ags corrections` 在历史对话里找到了它，还附上了机器人当时给的那份只来自新闻的清单。整理成记忆以后，所有机器人做选题调研时都会先把范围铺开。

```sh
ags corrections --since 7d
ags mistake add "选题调研只看了新闻" -l "先列全所有题材类型，再用数据收窄" -t 调研
```

### 4. 发现过时的记忆

有一条记忆写着「小红书运维已停，恢复前不发帖」，实际上几天前就恢复了。夜间整理读当天的对话（`ags digest`）时发现了这个矛盾，于是用新条目替代了旧条目：

```sh
ags mem add project xhs-ops -d "11-02 前后已恢复，四个定时任务每次开新会话"
ags mem supersede xhs-ops-stopped xhs-ops
```

旧条目不会删，标成 `superseded`，从此不再出现在索引里。改造之前，两个机器人都会按旧的那条办事。

### 5. 新项目三十秒搭好空间

```sh
cd my-project
ags init --lang zh
```

会生成这些：

```text
AGENTS.md                 所有 Agent 的规矩（CLAUDE.md / GEMINI.md / QWEN.md 都软链接到它）
memory/USER.md            用户是谁、偏好（每次开工都读，3000 字上限）
memory/NOW.md             现在的优先级、等用户决定的事（2000 字上限）
memory/MEMORY.md          条目索引（自动生成）
memory/<类型>/<条目>.md    一事一文件：偏好纠正、项目、决策、资源
wiki/  mistakes/  journal/
.claude/settings.json     加一个钩子：每聊 10 轮提醒 Agent 复盘该记什么
.cursor/rules/            让 Cursor 也读同一份 AGENTS.md
```

已经存在的文件一律不覆盖，重复执行也没关系。

## 三档搜索

大多数时候，Agent 只需要知道人和 Agent 之间说了什么，比如定过什么、有什么偏好；只有排查问题时才要看细节。所以检索分三档：

| `--level` | 搜什么 | 适合 |
|---|---|---|
| `chat`（默认） | 人和 Agent 之间说的话 | 查决定、偏好、「我们当时怎么说的」 |
| `work` | 再加上跑过的命令、改过的文件 | 「哪个会话改过 nav.css」「谁跑过迁移」 |
| `full` | 再加上工具输出 | 查报错信息、堆栈、日志 |

不写 `--level` 时先按 `chat` 搜。对话里搜不到，会自动升到 `full` 再搜一遍，并注明结果来自命令和输出。每条结果都会标出是在哪一层搜到的。MCP 的 `search_sessions` 也支持 `level` 参数。

如果只需要对话，可以在配置里写 `[index] store_output = false`，不存工具输出，索引会小一些。

## 能起到什么作用

- **多 Agent 接力**：任何一个 Agent 都能接住另一个的活，不管它们用的是什么工具、什么模型。
- **少交代、少返工**：用户说过一次的偏好和纠正，所有 Agent 下次都知道。
- **记忆能长期维护**：有字数上限、有替代记录、有体检，不会越堆越乱。
- **历史变成知识库**：`ags wiki export` 把对话导出成 Markdown，每个会话一页、每天一页汇总，可以直接放进 Obsidian 或 git。
- **能接进自动化**：每个命令都可以加 `--json`；`ags mcp` 提供 MCP 接口；`ags doctor` 有问题就返回失败，可以挂进定时任务当关卡。

## 安全和隐私

- 只读各家 Agent 的文件，从不改动。
- 不联网、不上报、不调大模型。
- 存进索引和打印出来之前，先去掉私钥、各类 API Key、JWT、Bearer token、`password=` 这类赋值、看起来随机的长串、手机号和身份证号。
- 索引文件权限是 600，只有本人能读。

## 支持的 Agent

| Agent | 状态 |
|---|---|
| Claude Code、Cursor（CLI 和 IDE）、OpenAI Codex、Hermes Agent、Pi | 在真实数据上测过 |
| DeepSeek Harness（dsh）、Gemini CLI、Qwen Code、OpenCode 2.x | 用样例数据测过 |

`ags agents` 会显示本机找到了哪些。加一个新 Agent 只要在 `src/connectors/` 里加一个文件，做法见 [docs/DESIGN.md](docs/DESIGN.md)。

## 安装

```sh
brew install yongvae/tap/agentstory
```

已在 macOS（Apple 芯片）、Homebrew 7 上实际安装验证：直接下载预编译包，几秒装好，`brew test` 通过。

会装上两个命令：`agentstory` 和短别名 `ags`。macOS（Apple 芯片、Intel）和 Linux（x86_64、arm64）的预编译包在 [Releases 页面](https://github.com/yongvae/agentstory/releases)。

**让 Agent 自己用，最简单的办法是直接跟它说**：

> 帮我安装 https://github.com/yongvae/agentstory ，用来搜索我的多 Agent 历史对话

Agent 会装好程序，并把仓库里的 [`skills/agentstory/`](skills/agentstory/SKILL.md) 复制进自己的 skills 目录。不用单独建索引，第一次运行任何命令都会自动建。这个 skill 告诉 Agent 什么时候该先搜、怎么读命中的那段对话、接手别人的会话前先看交接单。手动安装：

```sh
brew install yongvae/tap/agentstory
git clone --depth 1 https://github.com/yongvae/agentstory /tmp/agentstory
cp -r /tmp/agentstory/skills/agentstory ~/.claude/skills/    # Claude Code
# 其他支持 Agent Skills 的 Agent，换成它的 skills 目录，比如 ~/.agents/skills/
```

不支持 skill 的 Agent，可以用 MCP（`claude mcp add agentstory -- ags mcp`，Codex、Cursor、Gemini 等配置方法相同），或者在 AGENTS.md 里写一句「问到过去的事，先 `ags search` 再回答」。

## 常用命令速查

| 命令 | 作用 |
|---|---|
| `ags search 词 [--level chat\|work\|full] [-a codex] [--since 7d] [--only notes]` | 检索会话和知识 |
| `ags sessions [--since today] [-g 词]` | 列出会话 |
| `ags show <id> [--last 3 \| --around 词]` | 读会话 |
| `ags handoff <id>` | 出交接单 |
| `ags init [--lang zh]` | 搭建 Agent 空间 |
| `ags mem add/supersede/list` | 管理记忆条目 |
| `ags mistake add` | 新增一条错题 |
| `ags wiki add` / `ags wiki export` | 新增 wiki 页面 / 把会话导出成 wiki |
| `ags doctor` | 体检 |
| `ags corrections` | 找出用户纠正过 Agent 的地方 |
| `ags digest 2026-11-04` | 导出某一天的对话摘要 |
| `ags mcp` | 启动 MCP 服务 |

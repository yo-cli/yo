# yo-file 产品需求规格

> 本文档面向 AI 开发助手，描述 yo-file 的产品定位、核心场景和设计决策。

---

## 1. 产品定位

**yo-file 是一个模板克隆工具。**

目标用户是经常需要从现有项目/模块复制出新项目/模块的开发者。直接复制目录后手动查找替换命名很容易遗漏，尤其是当同一个关键词以不同命名风格出现时（kebab-case、snake_case、PascalCase、camelCase、SCREAMING_SNAKE_CASE）。yo-file 自动识别所有命名变体并一次性全部替换。

核心理念：**复制一个模块，改个名字就能用。**

---

## 2. 核心场景

### 2.1 克隆目录模板

**需求：** 用户有一个现成的模块目录（如 `user-service/`），想复制一份改名为 `order-service/`，所有文件名和文件内容中的 "user-service"、"user_service"、"UserService"、"userService"、"USER_SERVICE" 都要对应替换。

**行为：**
- 用户运行 `yo-file clone`
- 交互式输入旧关键词（如 `user-service`）
- 交互式输入新关键词（如 `order-service`）
- 交互式输入源路径（支持文件或目录）
- 系统自动推导所有命名变体：
  - `user-service` → `order-service`（kebab-case）
  - `user_service` → `order_service`（snake_case）
  - `UserService` → `OrderService`（PascalCase）
  - `userService` → `orderService`（camelCase）
  - `USER_SERVICE` → `ORDER_SERVICE`（SCREAMING_SNAKE_CASE）
- 递归复制目录，替换文件名和文本文件内容中的所有匹配项
- 二进制文件原样复制，不做替换
- 显示变更预览，用户确认后执行

### 2.2 克隆单个文件

**需求：** 有时只需要复制一个文件，如从 `UserController.java` 复制出 `OrderController.java`。

**行为：**
- 同上流程，源路径指向单个文件
- 输出文件名中的关键词也被替换

---

## 3. 设计决策

| 决策 | 理由 |
|------|------|
| 自动推导命名变体 | 用户只需输入一种形式，系统推导全部 5 种风格，避免遗漏 |
| 智能输入格式识别 | 无论用户输入 kebab-case、snake_case 还是 PascalCase，都能正确拆词 |
| 词边界匹配替换 | 避免误替换（如 "user" 不会替换 "username" 中的 "user"） |
| 文本文件启发式检测 | 通过扩展名列表 + 空字节检测判断是否为文本文件，二进制文件跳过内容替换 |
| 交互式确认 | 显示完整变更列表后才执行，防止误操作 |

---

## 4. 当前已知限制

- **仅支持 clone 子命令：** 文件工具目前只有克隆功能，未来可扩展其他文件操作
- **不支持正则自定义：** 替换规则固定为 5 种命名变体，不支持用户自定义模式
- **无 .gitignore 感知：** 会复制所有文件，包括 node_modules、target 等应忽略的目录

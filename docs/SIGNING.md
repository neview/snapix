# Windows 代码签名

签名解决两件事：

1. 安装时不再弹 **SmartScreen「未知发布者」**；
2. 自动更新下载的安装包不会被 SmartScreen / 杀软拦下导致更新失败。

> 证书需要真金白银买，代码侧无法凭空完成。仓库里已经把「拿到证书之后的所有接线」做完了，
> 你只需要买证书、配两个 secret。

## 一、买什么证书

| 类型 | 价格量级 | 说明 |
|---|---|---|
| **OV 代码签名证书** | 约 ¥1500–3000 / 年 | 最常用。需要企业营业执照 + 电话核验，1–3 天下发。 |
| **EV 代码签名证书** | 约 ¥3000–6000 / 年 | 需要 USB 硬件令牌（或云签名服务）。SmartScreen 信誉起量最快，但 CI 里必须用云签名 / 硬件网关。 |
| **Azure Trusted Signing** | 按签名次数计费 | 微软自己的云签名服务，CI 集成最省事，需要企业资质审核。 |

个人开发者通常选 **OV + 云签名服务**（如 DigiCert KeyLocker、SSL.com eSigner），
这样 CI 里不需要插 U 盘。

## 二、接进来（OV + PFX）

### 1. 本地打包

```powershell
$env:SNAPIX_SIGN_PFX = "D:\certs\snapix.pfx"
$env:SNAPIX_SIGN_PFX_PASSWORD = "你的密码"
pnpm build:signed
```

`build:signed` 比普通 `pnpm tauri build` 多传一个 `--config src-tauri/tauri.signing.conf.json`，
那份配置只做一件事：把 `signCommand` 指向 `scripts/sign-windows.mjs`。

脚本行为：

- 没有 `SNAPIX_SIGN_PFX` / `SNAPIX_SIGN_PFX_BASE64` → 打印「未配置证书，跳过」并 **exit 0**，构建照常成功；
- 有证书 → 调 `signtool sign /fd sha256 /tr <时间戳> /td sha256`，签名失败会让构建失败（不会悄悄出一版没签的包）。

`signtool.exe` 默认自动从 `C:\Program Files (x86)\Windows Kits\10\bin\<版本>\x64\` 找，
也可以显式指定：

```powershell
$env:SIGNTOOL = "C:\Program Files (x86)\Windows Kits\10\bin\10.0.22621.0\x64\signtool.exe"
```

### 2. CI 发布

在仓库 Secrets 里加两项（Settings → Secrets and variables → Actions）：

| Secret | 内容 |
|---|---|
| `SNAPIX_SIGN_PFX_BASE64` | PFX 的 base64：`[Convert]::ToBase64String([IO.File]::ReadAllBytes("snapix.pfx"))` |
| `SNAPIX_SIGN_PFX_PASSWORD` | PFX 密码 |

配好后推 `v*` tag，`.github/workflows/release.yml` 会自动带上
`--config src-tauri/tauri.signing.conf.json` 并注入这两个环境变量；没配就保持现在的无签名构建。

> **顺序很关键**：签名必须发生在 Tauri 打包过程中（`signCommand`），
> 不能「打包完再签」。因为 `latest.json` 里的更新包签名是在打包末尾对最终安装包计算的，
> 事后改文件会让 `.sig` 失效，客户端会拒绝更新。

## 三、换成云签名 / EV 令牌

`signCommand` 就是一整条命令行，把 `tauri.signing.conf.json` 里的值换掉即可，例如 Azure Trusted Signing：

```json
{
  "bundle": {
    "windows": {
      "signCommand": "trusted-signing-cli -e https://eus.codesigning.azure.net -a <账号> -c <证书配置> -d Snapix %1"
    }
  }
}
```

EV 硬件令牌在 CI 里无法直接使用，需要厂商的云签名网关（DigiCert KeyLocker 的 `smctl`、
SSL.com 的 `esigner` 等），同样是替换这一行。

## 四、验证签名

```powershell
signtool verify /pa /v src-tauri\target\release\bundle\nsis\Snapix_0.6.x_x64-setup.exe
```

看到 `Successfully verified` 即成功；安装时发布者应显示为你的公司名。

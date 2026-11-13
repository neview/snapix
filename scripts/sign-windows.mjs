#!/usr/bin/env node
/**
 * Windows 代码签名（可选）。
 *
 * Tauri 打包时会把每个待签文件路径作为参数传进来（`signCommand` 里的 `%1`）。
 * 没有配置证书时**直接跳过并退出 0**，保证本地 / CI 的普通构建不受影响；
 * 配好证书后同一份配置就会开始签名，不需要再改 tauri.conf.json。
 *
 * 需要的环境变量（任一即可）：
 *   SNAPIX_SIGN_PFX           PFX 文件路径
 *   SNAPIX_SIGN_PFX_BASE64    PFX 的 base64（CI 里用 secret 存这一份，落地到临时文件）
 * 其它可选：
 *   SNAPIX_SIGN_PFX_PASSWORD  证书密码（默认空）
 *   SNAPIX_SIGN_TIMESTAMP     时间戳服务（默认 DigiCert）
 *   SIGNTOOL                  signtool.exe 路径（默认自动找 Windows SDK）
 *
 * 用法：node scripts/sign-windows.mjs <文件>
 */

import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const file = process.argv[2];
const pfxPath = process.env.SNAPIX_SIGN_PFX?.trim();
const pfxB64 = process.env.SNAPIX_SIGN_PFX_BASE64?.trim();
const password = process.env.SNAPIX_SIGN_PFX_PASSWORD ?? "";
const timestamp = process.env.SNAPIX_SIGN_TIMESTAMP?.trim() || "http://timestamp.digicert.com";

if (!file) {
  console.error("[sign] 缺少待签文件参数");
  process.exit(1);
}
if (!pfxPath && !pfxB64) {
  console.log(`[sign] 未配置证书，跳过：${file}`);
  process.exit(0);
}

function findSigntool() {
  if (process.env.SIGNTOOL) return process.env.SIGNTOOL;
  const kits = "C:\\Program Files (x86)\\Windows Kits\\10\\bin";
  if (existsSync(kits)) {
    const versions = readdirSync(kits)
      .filter((v) => /^10\./.test(v))
      .sort()
      .reverse();
    for (const v of versions) {
      for (const arch of ["x64", "x86"]) {
        const p = join(kits, v, arch, "signtool.exe");
        if (existsSync(p)) return p;
      }
    }
  }
  return "signtool.exe"; // 交给 PATH
}

let dir = null;
let cert = pfxPath;
try {
  if (!cert && pfxB64) {
    dir = mkdtempSync(join(tmpdir(), "snapix-sign-"));
    cert = join(dir, "cert.pfx");
    writeFileSync(cert, Buffer.from(pfxB64, "base64"));
  }

  const args = [
    "sign",
    "/fd",
    "sha256",
    "/f",
    cert,
    "/p",
    password,
    "/tr",
    timestamp,
    "/td",
    "sha256",
    file,
  ];
  console.log(`[sign] 签名 ${file}`);
  execFileSync(findSigntool(), args, { stdio: "inherit" });
  console.log(`[sign] 完成 ${file}`);
} catch (e) {
  console.error(`[sign] 签名失败：${e?.message ?? e}`);
  process.exit(1);
} finally {
  // 临时导出的私钥用完即删
  if (dir) {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      // 删不掉也不影响构建，系统临时目录会自然回收
    }
  }
}

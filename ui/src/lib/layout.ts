// 归档模板：与 crates/core/src/layout.rs 的规则一致，用于设置页的即时预览与校验（保存时后端会再校验一次）
import type { FolderLayout } from "./api";

export const VARIABLES: { name: string; label: string; hint: string }[] = [
  { name: "year", label: "年", hint: "{year}，如 2026" },
  { name: "month", label: "月", hint: "{month}，如 10" },
  { name: "day", label: "日", hint: "{day}，如 20" },
  { name: "date", label: "日期", hint: "{date}，如 2026-10-20" },
  { name: "camera", label: "相机", hint: "{camera}，取自照片 EXIF 的相机型号" },
  { name: "card", label: "卡名", hint: "{card}，存储卡的卷名" },
];

export const PRESETS: Record<Exclude<FolderLayout, "custom">, string> = {
  date: "{year}/{month}/{day}",
  camera: "{camera}/{year}/{month}/{day}",
  card: "{card}/{year}/{month}/{day}",
  date_camera: "{year}/{month}/{day}/{camera}",
  date_card: "{year}/{month}/{day}/{card}",
};

const SAMPLE: Record<string, string> = {
  year: "2026",
  month: "10",
  day: "20",
  date: "2026-10-20",
  camera: "Canon EOS R5",
  card: "EOS_DIGITAL",
};

function sanitize(s: string): string {
  const t = s
    .replace(/[/:\\\p{Cc}]/gu, "-")
    .trim()
    .replace(/^\.+/, "")
    .trim();
  return t || "未命名";
}

/** 用示例值渲染模板，返回相对路径；模板无效时返回错误说明 */
export function renderTemplate(template: string, file = "IMG_4821.CR3"): { path: string } | { error: string } {
  const levels: string[] = [];
  for (const level of template.split("/")) {
    let out = "";
    let blank = true;
    for (let i = 0; i < level.length; i++) {
      const c = level[i];
      if (c === "{") {
        const end = level.indexOf("}", i);
        if (end < 0) return { error: "有一个「{」没有闭合" };
        const name = level.slice(i + 1, end).trim().toLowerCase();
        if (!(name in SAMPLE)) {
          return { error: `不认识的变量 {${name}}，可用：${VARIABLES.map((v) => `{${v.name}}`).join(" ")}` };
        }
        out += SAMPLE[name];
        blank = false;
        i = end;
      } else if (c === "}") {
        return { error: "多了一个「}」" };
      } else {
        out += c;
        if (c.trim()) blank = false;
      }
    }
    if (!blank) levels.push(sanitize(out));
  }
  levels.push(file);
  return { path: levels.join("/") };
}

export function templateOf(layout: FolderLayout, custom: string): string {
  return layout === "custom" ? custom : PRESETS[layout];
}

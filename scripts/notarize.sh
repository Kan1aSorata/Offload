#!/usr/bin/env bash
# 提交 Apple 公证并等待结果，通过后装订票据。
# 用法：scripts/notarize.sh <提交的文件> [装订的文件，默认同提交的文件]
# 需要环境变量：APPLE_API_KEY_PATH（.p8 文件路径）、APPLE_API_KEY_ID、APPLE_API_ISSUER
set -euo pipefail

submit="$1"
staple="${2:-$1}"
auth=(--key "$APPLE_API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER")

echo "提交公证：$submit"
out=$(xcrun notarytool submit "$submit" "${auth[@]}" --wait --timeout 2h --output-format json)
echo "$out"
id=$(jq -r '.id' <<<"$out")
status=$(jq -r '.status' <<<"$out")

if [[ "$status" != "Accepted" ]]; then
  echo "::error::公证未通过（$status），以下是 Apple 返回的日志"
  xcrun notarytool log "$id" "${auth[@]}" || true
  exit 1
fi

xcrun stapler staple "$staple"
xcrun stapler validate "$staple"

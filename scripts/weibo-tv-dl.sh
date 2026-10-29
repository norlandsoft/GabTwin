#!/usr/bin/env bash
# 微博视频（weibo.com/tv/show/...）直链解析 / 下载
#
#   ./weibo-tv-dl.sh <url>            列出所有清晰度的直链
#   ./weibo-tv-dl.sh <url> -d         下载最高清晰度到当前目录
#   ./weibo-tv-dl.sh <url> -d 720P    下载指定清晰度（480P/720P/1080P/2K）
#
# 原理：拿一串游客 cookie（passport.weibo.com/visitor），再请求
# /tv/api/component 的 Component_Play_Playinfo，返回 JSON 里 urls 就是 mp4 直链。
# 直链带 Expires/ssig 签名，约 1 小时后失效，必须现取现下。
set -euo pipefail

URL_IN="${1:-}"
[ -n "$URL_IN" ] || { sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }
shift
DO_DOWNLOAD=0
WANT=""
while [ $# -gt 0 ]; do
  case "$1" in
    -d|--download) DO_DOWNLOAD=1 ;;
    *) WANT="$1" ;;
  esac
  shift
done

OID=$(printf '%s' "$URL_IN" | grep -oE '[0-9]{4}:[0-9]{6,}' | head -1 || true)
[ -n "$OID" ] || { echo "无法从 URL 里认出视频 oid（形如 1034:5336234839441468）" >&2; exit 2; }

UA='Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36'
TMP=$(mktemp -d); trap 'rm -rf "$TMP"' EXIT

# 1) 游客 cookie
RESP=$(curl -sS -m 20 -A "$UA" -c "$TMP/cj" 'https://passport.weibo.com/visitor/genvisitor' \
  --data-urlencode 'cb=gen_callback' \
  --data-urlencode 'fp={"os":"1","browser":"Chrome124,0,0,0","fonts":"undefined","screenInfo":"1920*1080*24","plugins":""}')
TID=$(printf '%s' "$RESP" | python3 -c 'import sys,json,re; print(json.loads(re.search(r"\{.*\}",sys.stdin.read()).group(0))["data"]["tid"])')
curl -sS -m 20 -A "$UA" -b "$TMP/cj" -c "$TMP/cj" \
  "https://passport.weibo.com/visitor/visitor?a=incarnate&t=${TID}&w=2&c=095&gc=&cb=cross_domain&from=weibo&_rand=0.5" -o /dev/null

# 2) 取播放信息
curl -sS -m 25 -A "$UA" -b "$TMP/cj" \
  -H "Referer: https://weibo.com/tv/show/${OID}" -H 'X-Requested-With: XMLHttpRequest' \
  --data-urlencode "data={\"Component_Play_Playinfo\":{\"oid\":\"${OID}\"}}" \
  "https://weibo.com/tv/api/component?page=/tv/show/${OID}" -o "$TMP/api.json"

python3 - "$TMP/api.json" "$OID" "$DO_DOWNLOAD" "$WANT" <<'PY'
import json, os, subprocess, sys

path, oid, dl, want = sys.argv[1:5]
dl = (dl == "1")
d = json.load(open(path))["data"]["Component_Play_Playinfo"]
title = (d.get("title") or oid).strip() or oid
urls = {"2K": None, "1080P": None, "720P": None, "480P": None}
for k, v in d.get("urls", {}).items():
    key = k.split()[-1]
    if key in urls:
        urls[key] = "https:" + v if v.startswith("//") else v

if not dl:
    print(f"# {title}  ({d.get('duration_time') or ''})  {oid}")
    for k, u in urls.items():
        if u:
            print(f"{k:>6}  {u}")
    sys.exit(0)

pick = (want or "2K").upper()
u = urls.get(pick) or next((x for x in urls.values() if x), None)
if not u:
    sys.exit("没有可用直链")
safe = "".join(c for c in title if c not in '/\\:*?"<>|').strip() or oid
out = f"{safe}_{pick}.mp4"
# 注意：CDN 对浏览器 UA 反而回 403，这里用 curl 默认 UA
subprocess.run(["curl", "-sS", "--retry", "2", "-m", "600", "-o", out, u], check=True)
print(f"已保存: {os.path.abspath(out)}  ({os.path.getsize(out)/1048576:.1f} MB)")
PY

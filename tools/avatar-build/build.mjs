// 把 three + 数字人源码打成一个离线可用的 IIFE。
// 产物 ui/vendor/avatar.js 要提交进仓库：应用不依赖 CDN，也不要求每次启动都 build。
import { build } from "esbuild";

await build({
  entryPoints: ["src/main.js"],
  bundle: true,
  format: "iife",
  globalName: "GabTwinAvatar",
  target: ["safari17"],
  outfile: "../../ui/vendor/avatar.js",
  minify: true,
  sourcemap: false,
  legalComments: "none",
  logLevel: "info",
});

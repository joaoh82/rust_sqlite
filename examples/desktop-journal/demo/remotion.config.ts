// Remotion CLI config. The defaults are sane for our use case; we only
// turn on the modern bundler features and tell Remotion to walk
// `public/` (where the screenshot copies live during dev) without
// caching across renders.
import { Config } from "@remotion/cli/config";

Config.setVideoImageFormat("jpeg");
Config.setPixelFormat("yuv420p"); // QuickTime / web-friendly H.264
Config.setOverwriteOutput(true);

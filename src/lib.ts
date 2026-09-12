export function derivedWidth(sourceWidth: number, sourceHeight: number, squeeze: number, targetHeight: number) {
  const raw = (sourceWidth / sourceHeight) * squeeze * targetHeight;
  return Math.round(raw / 2) * 2;
}

export function proxyPath(sourceDir: string, suffix = "_proxy") {
  const separator = sourceDir.includes("\\") ? "\\" : "/";
  return `${sourceDir.replace(/[\\/]$/, "")}${separator}Proxy`;
}

export function basename(path: string) {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}


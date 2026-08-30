export type SupportedFormat = {
  extension: string;
  label: string;
};

export function fileExtension(path: string): string | null {
  const name = path.split(/[\\/]/).pop() ?? path;
  const dot = name.lastIndexOf(".");
  if (dot < 0 || dot === name.length - 1) return null;
  return name.slice(dot + 1);
}

export function isSupported(path: string, formats: SupportedFormat[]) {
  const extension = fileExtension(path)?.toLowerCase();
  return extension !== undefined && formats.some((format) => format.extension === extension);
}

export function partitionSupported(paths: string[], formats: SupportedFormat[]) {
  const accepted: string[] = [];
  const rejected: string[] = [];

  for (const path of paths) {
    (isSupported(path, formats) ? accepted : rejected).push(path);
  }

  return { accepted, rejected };
}

export function supportedFormatLabels(formats: SupportedFormat[]) {
  return formats.map((format) => format.label).join(", ");
}

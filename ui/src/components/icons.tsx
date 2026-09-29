// 内联 SVG 图标（白名单无图标库依赖；stroke 用 currentColor 继承文字色）。
// 尺寸默认 20px；语义不依赖颜色单独传达（§4.9：禁仅用颜色传达语义）。
import type { ReactNode, SVGProps } from "react";

type IconProps = SVGProps<SVGSVGElement> & { size?: number };

function base(size: number | undefined, children: ReactNode, props: SVGProps<SVGSVGElement>) {
  return (
    <svg
      width={size ?? 20}
      height={size ?? 20}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {children}
    </svg>
  );
}

export function IconDashboard(p: IconProps) {
  return base(p.size, <>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <path d="M3 9h18M9 21V9" />
  </>, p);
}

export function IconKeyboard(p: IconProps) {
  return base(p.size, <>
    <rect x="2" y="6" width="20" height="12" rx="2" />
    <path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M6 14h.01M18 14h.01M9 14h6" />
  </>, p);
}

export function IconMouse(p: IconProps) {
  return base(p.size, <>
    <rect x="7" y="2.5" width="10" height="19" rx="5" />
    <path d="M12 6v4" />
  </>, p);
}

export function IconGamepad(p: IconProps) {
  return base(p.size, <>
    <path d="M6.5 7h11a4.5 4.5 0 0 1 4.4 5.4l-.9 4.2a2.6 2.6 0 0 1-4.6 1L15 16H9l-1.4 1.6a2.6 2.6 0 0 1-4.6-1l-.9-4.2A4.5 4.5 0 0 1 6.5 7Z" />
    <path d="M8 11v3M6.5 12.5h3M15.5 11.5h.01M17.5 13.5h.01" />
  </>, p);
}

export function IconApps(p: IconProps) {
  return base(p.size, <>
    <rect x="3" y="4" width="18" height="14" rx="2" />
    <path d="M3 8h18M7 21h10" />
  </>, p);
}

export function IconCombos(p: IconProps) {
  return base(p.size, <>
    <rect x="2.5" y="8" width="12" height="9" rx="1.5" />
    <rect x="9.5" y="4" width="12" height="9" rx="1.5" />
    <path d="M12.5 8.5h3" />
  </>, p);
}

export function IconPulse(p: IconProps) {
  return base(p.size, <>
    <path d="M2 12h4l2.5-7 4 14 3-9 1.8 2H22" />
  </>, p);
}

export function IconSettings(p: IconProps) {
  return base(p.size, <>
    <circle cx="12" cy="12" r="3.2" />
    <path d="M12 2.8v2.4M12 18.8v2.4M4.9 4.9l1.7 1.7M17.4 17.4l1.7 1.7M2.8 12h2.4M18.8 12h2.4M4.9 19.1l1.7-1.7M17.4 6.6l1.7-1.7" />
  </>, p);
}

export function IconPlay(p: IconProps) {
  return base(p.size, <path d="M7 4.8v14.4L19 12 7 4.8Z" />, p);
}

export function IconPause(p: IconProps) {
  return base(p.size, <path d="M8 4.5v15M16 4.5v15" />, p);
}

export function IconDownload(p: IconProps) {
  return base(p.size, <path d="M12 3v11m0 0 4-4m-4 4-4-4M4 19h16" />, p);
}

export function IconImport(p: IconProps) {
  return base(p.size, <path d="M12 14V3m0 0 4 4m-4-4L8 7M4 19h16M4 16v3m16-3v3" />, p);
}

export function IconDatabase(p: IconProps) {
  return base(p.size, <>
    <ellipse cx="12" cy="5.5" rx="8" ry="3" />
    <path d="M4 5.5v13c0 1.7 3.6 3 8 3s8-1.3 8-3v-13M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3" />
  </>, p);
}

export function IconRefresh(p: IconProps) {
  return base(p.size, <path d="M20 12a8 8 0 1 1-2.3-5.6M20 3v4h-4" />, p);
}

export function IconCheck(p: IconProps) {
  return base(p.size, <path d="M4 12.5 9.5 18 20 6.5" />, p);
}

export function IconWarn(p: IconProps) {
  return base(p.size, <>
    <path d="M12 3 2.5 20h19L12 3Z" />
    <path d="M12 10v4M12 17.2h.01" />
  </>, p);
}

export function IconPlug(p: IconProps) {
  return base(p.size, <>
    <path d="M9 3v5M15 3v5M6.5 8h11v3a5.5 5.5 0 0 1-11 0V8ZM12 16.5V21" />
  </>, p);
}

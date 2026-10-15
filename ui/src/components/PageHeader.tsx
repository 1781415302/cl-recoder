// 页面顶栏：标题 + 说明在左，操作区（日期范围等）在右；滚动时吸顶。
import type { ReactNode } from "react";

export function PageHeader(props: {
  title: string;
  description?: string;
  actions?: ReactNode;
}) {
  return (
    <header className="topbar">
      <div className="topbar-copy">
        <h1 className="topbar-title">{props.title}</h1>
        {props.description ? (
          <p className="topbar-desc">{props.description}</p>
        ) : null}
      </div>
      {props.actions ? <div className="topbar-actions">{props.actions}</div> : null}
    </header>
  );
}

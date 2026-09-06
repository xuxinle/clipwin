// 待办日历弹窗：月份网格 + 快捷今天/明天/下周 + 清除
// 定位策略：fixed（脱离 overflow 滚动容器，永不被裁切）——anchorRect 为锚点按钮屏幕坐标，
// 自动判断向下/向上弹出（视口边界自适应）、水平对齐锚点右缘。
import { useEffect, useLayoutEffect, useRef, useState } from "react";

const WEEK = ["一", "二", "三", "四", "五", "六", "日"];
const DAY_MS = 86_400_000;

function ymdOf(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
function day0(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

export default function CalendarPopover({
  value, // 当前选中的 ymd | null
  anchorRect, // 锚点按钮 getBoundingClientRect()；null = 流内定位（编辑弹窗内旧用法兜底）
  onPick, // (ymd: string | null) => void；null = 清除
  onClose,
}: {
  value: string | null;
  anchorRect?: DOMRect | null;
  onPick: (ymd: string | null) => void;
  onClose: () => void;
}) {
  const today = new Date();
  const initial = value ? new Date(value + "T00:00:00") : today;
  const [view, setView] = useState({ y: initial.getFullYear(), m: initial.getMonth() });
  const popRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  // fixed 定位计算：首次渲染后测量弹窗尺寸，锚点下方优先、不够则上方；右缘对齐锚点右缘，越界夹回
  useLayoutEffect(() => {
    if (!anchorRect) return;
    const el = popRef.current;
    if (!el) return;
    const w = el.offsetWidth, h = el.offsetHeight;
    const vw = window.innerWidth, vh = window.innerHeight;
    // 水平：锚点居中对齐（弹窗场景锚点偏左也不外溢），再夹回视口
    let left = anchorRect.left + anchorRect.width / 2 - w / 2;
    if (left < 8) left = 8;
    if (left + w > vw - 8) left = vw - w - 8;
    let top = anchorRect.bottom + 6; // 下方优先
    if (top + h > vh - 8) top = Math.max(8, anchorRect.top - h - 6); // 放不下则上方
    setPos({ left, top });
  }, [anchorRect]);

  // Esc 关闭 + 点击外部关闭（fixed 层不依赖父级冒泡）
  useEffect(() => {
    if (!anchorRect) return;
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    const onDown = (e: MouseEvent) => {
      if (popRef.current && !popRef.current.contains(e.target as Node)) onClose();
    };
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("mousedown", onDown, true);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("mousedown", onDown, true);
    };
  }, [anchorRect, onClose]);

  const first = new Date(view.y, view.m, 1);
  // 周一起始：周一=0 … 周日=6
  const lead = (first.getDay() + 6) % 7;
  const dim = new Date(view.y, view.m + 1, 0).getDate();
  const todayYmd = ymdOf(today);
  const tomorrowYmd = ymdOf(new Date(day0(today) + DAY_MS));
  const nextWeekYmd = ymdOf(new Date(day0(today) + 7 * DAY_MS));

  const move = (delta: number) => {
    const d = new Date(view.y, view.m + delta, 1);
    setView({ y: d.getFullYear(), m: d.getMonth() });
  };

  const style = anchorRect
    ? { position: "fixed" as const, left: pos?.left, top: pos?.top, visibility: pos ? ("visible" as const) : ("hidden" as const) }
    : undefined;

  return (
    <div ref={popRef} className="calpop" style={style} onClick={(e) => e.stopPropagation()}>
      <div className="cal-head">
        <button className="cal-nav" onClick={() => move(-1)} title="上个月">‹</button>
        <span className="cal-title">{view.y} 年 {view.m + 1} 月</span>
        <button className="cal-nav" onClick={() => move(1)} title="下个月">›</button>
      </div>
      <div className="cal-grid cal-week">
        {WEEK.map((w) => <span key={w} className="cal-wd">{w}</span>)}
      </div>
      <div className="cal-grid">
        {Array.from({ length: lead }).map((_, i) => <span key={"e" + i} />)}
        {Array.from({ length: dim }).map((_, i) => {
          const ymd = `${view.y}-${String(view.m + 1).padStart(2, "0")}-${String(i + 1).padStart(2, "0")}`;
          const cls = [
            "cal-day",
            ymd === todayYmd ? "today" : "",
            ymd === value ? "sel" : "",
          ].filter(Boolean).join(" ");
          return (
            <button key={ymd} className={cls} onClick={() => onPick(ymd)}>
              {i + 1}
            </button>
          );
        })}
      </div>
      <div className="cal-quick">
        <button onClick={() => onPick(todayYmd)}>今天</button>
        <button onClick={() => onPick(tomorrowYmd)}>明天</button>
        <button onClick={() => onPick(nextWeekYmd)}>下周</button>
        {value && <button className="cal-clear" onClick={() => onPick(null)}>清除</button>}
        <span className="cal-space" />
        <button onClick={onClose}>✕</button>
      </div>
    </div>
  );
}

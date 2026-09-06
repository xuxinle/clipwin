// 待办编辑弹窗：标题 + 富文本内容（contenteditable 工具栏）+ 优先级/标签/日期 UI
import { useEffect, useRef, useState } from "react";
import CalendarPopover from "./CalendarPopover";

export interface TodoEditDraft {
  id: number | null; // null = 新建（尚未入库）
  title: string;
  contentHtml: string; // 富文本内容（HTML 片段）
  priority: number;
  due_at: number | null;
  tags: string[]; // 编辑用数组
  // 附加数据（图片/文件待办的原始内容展示）
  imageSrc?: string | null;
  files?: string[];
  rawText?: string | null; // 原始纯文本（text 字段）
}

const PRIO_OPTS: { v: number; label: string; icon: string }[] = [
  { v: 3, label: "高", icon: "🔴" },
  { v: 2, label: "中", icon: "🟡" },
  { v: 1, label: "低", icon: "🟢" },
  { v: 0, label: "无", icon: "🏳️" },
];

function fmtDue(ms: number | null): string {
  if (ms == null) return "";
  const d = new Date(ms);
  const today0 = new Date();
  const t0 = new Date(today0.getFullYear(), today0.getMonth(), today0.getDate()).getTime();
  const diff = Math.round((new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() - t0) / 86_400_000);
  if (diff === 0) return "今天";
  if (diff === 1) return "明天";
  if (diff < 0) return `逾期 ${-diff} 天`;
  return `${d.getMonth() + 1}/${d.getDate()}`;
}

export default function TodoEditor({
  draft, // 弹窗数据（父组件 state；null = 关闭）
  isNew, // 新建场景：默认聚焦「保存」——无需补充内容时回车直接创建
  onField, // 字段变更回调（draft 内字段）
  onSave, // 保存：标题/富文本/纯文本/优先级/日期/标签一次性提交
  onClose,
}: {
  draft: TodoEditDraft;
  isNew?: boolean;
  onField: (patch: Partial<TodoEditDraft>) => void;
  onSave: () => void;
  onClose: () => void;
}) {
  const contentRef = useRef<HTMLDivElement>(null);
  const [calOpen, setCalOpen] = useState(false);
  const [calAnchor, setCalAnchor] = useState<DOMRect | null>(null);
  const [tagInput, setTagInput] = useState("");
  const titleRef = useRef<HTMLInputElement>(null);
  const saveRef = useRef<HTMLButtonElement>(null);

  // 打开弹窗时：填入已有内容；新建聚焦「保存」（回车即建），编辑聚焦标题
  useEffect(() => {
    if (contentRef.current) contentRef.current.innerHTML = draft.contentHtml || "";
    if (isNew) saveRef.current?.focus();
    else { titleRef.current?.focus(); titleRef.current?.select(); }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft.id]);

  const exec = (cmd: string, val?: string) => {
    const el = contentRef.current;
    if (!el) return;
    el.focus();
    // 兜底：若点击导致选区丢失（应已被 onMouseDown preventDefault 保护），
    // 退化处理——无选区时 execCommand 作用于光标处（如列表按钮），仍有效
    document.execCommand(cmd, false, val);
    // 同步 state（innerHTML 变化不触发 React onInput 的场景）
    onField({ contentHtml: el.innerHTML });
  };

  const addTag = () => {
    const t = tagInput.trim().replace(/^#/, "");
    if (t && !draft.tags.includes(t)) onField({ tags: [...draft.tags, t] });
    setTagInput("");
  };

  return (
    <div className="modal-mask" onClick={onClose}>
      <div
      className="modal"
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => { if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); onSave(); } }}
    >
        {/* 标题行 */}
        <div className="ed-title-row">
          <input
            ref={titleRef}
            className="ed-title"
            value={draft.title}
            onChange={(e) => onField({ title: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter") { e.preventDefault(); contentRef.current?.focus(); }
              if (e.key === "Escape") onClose();
            }}
            placeholder="待办标题…"
            spellCheck={false}
          />
        </div>

        {/* 附件区：图片/文件原始内容 */}
        {draft.imageSrc && (
          <div className="ed-attach">
            <img src={draft.imageSrc} alt="附件图片" />
          </div>
        )}
        {draft.files && draft.files.length > 0 && (
          <div className="ed-attach files">
            {draft.files.map((f) => <div key={f} className="ed-file" title={f}>📁 {f}</div>)}
          </div>
        )}

        {/* 富文本工具栏 */}
        <div className="ed-toolbar">
          <button onMouseDown={(e) => e.preventDefault()} onClick={() => exec("bold")} title="加粗"><b>B</b></button>
          <button onMouseDown={(e) => e.preventDefault()} onClick={() => exec("italic")} title="斜体"><i>I</i></button>
          <button onMouseDown={(e) => e.preventDefault()} onClick={() => exec("underline")} title="下划线"><u>U</u></button>
          <span className="ed-sep" />
          <button onMouseDown={(e) => e.preventDefault()} onClick={() => { const u = window.prompt("链接地址", "https://"); if (u) exec("createLink", u); }} title="链接">🔗</button>
          <button onMouseDown={(e) => e.preventDefault()} onClick={() => exec("insertUnorderedList")} title="列表">• —</button>
          <button onMouseDown={(e) => e.preventDefault()} onClick={() => exec("insertOrderedList")} title="编号">1.</button>
          <span className="ed-sep" />
          <button onMouseDown={(e) => e.preventDefault()} onClick={() => exec("removeFormat")} title="清除格式">⌫ᶠ</button>
        </div>

        {/* 富文本内容区 */}
        <div
          ref={contentRef}
          className="ed-content"
          contentEditable
          suppressContentEditableWarning
          onInput={() => onField({ contentHtml: contentRef.current?.innerHTML ?? "" })}
          onKeyDown={(e) => {
            if (e.key === "Escape") onClose();
            // Ctrl+Enter 任意位置保存
            if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); onSave(); }
          }}
          onPaste={(e) => {
            // 粘贴图片：读 blob 转内联 <img>（本地待办，体积可控）
            const items = e.clipboardData?.items;
            if (items) {
              for (const it of Array.from(items)) {
                if (it.type.startsWith("image/")) {
                  e.preventDefault();
                  const file = it.getAsFile();
                  if (!file) return;
                  const reader = new FileReader();
                  reader.onload = () => {
                    exec("insertImage", String(reader.result));
                  };
                  reader.readAsDataURL(file);
                  return;
                }
              }
            }
          }}
          data-ph="添加详细内容（支持加粗/列表/链接，可直接粘贴图片）…"
        />

        {/* 属性区：优先级 / 截止日 / 标签 */}
        <div className="ed-props">
          <div className="ed-prop">
            <span className="ed-plabel">优先级</span>
            <div className="seg">
              {PRIO_OPTS.map((p) => (
                <button
                  key={p.v}
                  className={draft.priority === p.v ? "seg-btn active" : "seg-btn"}
                  onClick={() => onField({ priority: p.v })}
                >{p.icon} {p.label}</button>
              ))}
            </div>
          </div>

          <div className="ed-prop">
            <span className="ed-plabel">截止日</span>
            <button
              className="ed-due"
              onClick={(e) => {
                setCalAnchor(e.currentTarget.getBoundingClientRect());
                setCalOpen(!calOpen);
              }}
            >
              📅 {draft.due_at != null ? fmtDue(draft.due_at) : "设置"}
            </button>
          </div>

          <div className="ed-prop ed-tags">
            <span className="ed-plabel">标签</span>
            {draft.tags.map((t) => (
              <span key={t} className="ed-tag">
                #{t}
                <button className="ed-tag-x" onClick={() => onField({ tags: draft.tags.filter((x) => x !== t) })}>✕</button>
              </span>
            ))}
            <input
              className="ed-tag-in"
              value={tagInput}
              onChange={(e) => setTagInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === ",") { e.preventDefault(); addTag(); }
                if (e.key === "Backspace" && !tagInput && draft.tags.length > 0) {
                  onField({ tags: draft.tags.slice(0, -1) });
                }
              }}
              onBlur={addTag}
              placeholder="+ 标签"
              spellCheck={false}
            />
          </div>
        </div>

        {/* 底部：保存/取消 */}
        <div className="ed-foot">
          <span className="ed-hint">Ctrl+Enter 保存 · Esc 取消</span>
          <div className="ed-btns">
            <button className="ed-cancel" onClick={onClose}>取消</button>
            <button ref={saveRef} className="ed-save" onClick={onSave}>保存</button>
          </div>
        </div>
        {calOpen && calAnchor && (
          <CalendarPopover
            value={draft.due_at != null ? new Date(draft.due_at).toISOString().slice(0, 10) : null}
            anchorRect={calAnchor}
            onPick={(ymd) => {
              onField({ due_at: ymd ? new Date(ymd + "T00:00:00").getTime() : null });
              setCalOpen(false);
            }}
            onClose={() => setCalOpen(false)}
          />
        )}
      </div>
    </div>
  );
}

import {
  useState,
  useRef,
  useEffect,
  useCallback,
  createContext,
  useContext,
  useId,
  isValidElement,
  cloneElement,
  type ReactNode,
  type ReactElement,
  type HTMLAttributes,
  type ButtonHTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { createPortal } from "react-dom";
import { ChevronDown, Check } from "lucide-react";
import { cn } from "@/utils/cn";

interface DropdownContextValue {
  open: boolean;
  setOpen: (open: boolean) => void;
  triggerRef: React.RefObject<HTMLButtonElement | null>;
  contentRef: React.RefObject<HTMLDivElement | null>;
  highlightedIndex: number;
  setHighlightedIndex: (index: number) => void;
  menuId: string;
  registerItem: () => number;
  itemCount: number;
  /** Most recent input modality. `mouseenter` only steals focus when the
   * user was already using the mouse — otherwise arrow keys would lose
   * the highlight as soon as the cursor drifted across an item. */
  inputModalityRef: React.RefObject<"keyboard" | "mouse">;
  setInputModality: (modality: "keyboard" | "mouse") => void;
}

const DropdownContext = createContext<DropdownContextValue | null>(null);

function useDropdownContext() {
  const context = useContext(DropdownContext);
  if (!context) {
    throw new Error("Dropdown components must be used within a Dropdown");
  }
  return context;
}

interface DropdownProps {
  children: ReactNode;
}

export function Dropdown({ children }: DropdownProps) {
  const [open, setOpenState] = useState(false);
  const [highlightedIndex, setHighlightedIndex] = useState(-1);
  const [itemCount, setItemCount] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const menuId = useId();
  const itemCounterRef = useRef(0);
  const inputModalityRef = useRef<"keyboard" | "mouse">("mouse");

  // Wrapper to reset state when opening
  const setOpen = useCallback((value: boolean) => {
    if (value) {
      itemCounterRef.current = 0;
      setHighlightedIndex(-1);
    }
    setOpenState(value);
  }, []);

  const registerItem = useCallback(() => {
    const index = itemCounterRef.current;
    itemCounterRef.current += 1;
    setItemCount(itemCounterRef.current);
    return index;
  }, []);

  const setInputModality = useCallback((modality: "keyboard" | "mouse") => {
    inputModalityRef.current = modality;
  }, []);

  return (
    <DropdownContext.Provider
      value={{
        open,
        setOpen,
        triggerRef,
        contentRef,
        highlightedIndex,
        setHighlightedIndex,
        menuId,
        registerItem,
        itemCount,
        inputModalityRef,
        setInputModality,
      }}
    >
      <div className="relative inline-block">{children}</div>
    </DropdownContext.Provider>
  );
}

interface DropdownTriggerProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  asChild?: boolean;
  showChevron?: boolean;
  /** Render as a borderless icon button (for MoreHorizontal / action triggers) */
  variant?: "default" | "ghost";
}

export function DropdownTrigger({
  className,
  children,
  asChild,
  showChevron = true,
  variant = "default",
  ...props
}: DropdownTriggerProps) {
  const { open, setOpen, triggerRef, menuId, setHighlightedIndex, itemCount } =
    useDropdownContext();

  const handleKeyDown = (e: ReactKeyboardEvent<HTMLButtonElement>) => {
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        if (!open) {
          setOpen(true);
          setHighlightedIndex(0);
        } else {
          setHighlightedIndex(0);
        }
        break;
      case "ArrowUp":
        e.preventDefault();
        if (!open) {
          setOpen(true);
          setHighlightedIndex(Math.max(0, itemCount - 1));
        } else {
          setHighlightedIndex(Math.max(0, itemCount - 1));
        }
        break;
      case "Enter":
      case " ":
        if (!open) {
          e.preventDefault();
          setOpen(true);
          setHighlightedIndex(0);
        }
        break;
    }
  };

  const handleClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    setOpen(!open);
  };

  // When asChild is true, merge trigger props directly onto the child element (Slot pattern)
  if (asChild) {
    if (isValidElement(children)) {
      const childProps = (children as ReactElement<Record<string, unknown>>).props;
      // Collect aria-* and title props from DropdownTrigger to pass through
      const passThroughProps: Record<string, unknown> = {};
      for (const [key, value] of Object.entries(props)) {
        if ((key.startsWith("aria-") || key === "title") && value !== undefined) {
          passThroughProps[key] = value;
        }
      }
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return cloneElement(children as ReactElement<any>, {
        ...passThroughProps,
        ref: triggerRef,
        onClick: (e: React.MouseEvent) => {
          (childProps.onClick as ((e: React.MouseEvent) => void) | undefined)?.(e);
          if (!e.defaultPrevented) handleClick(e);
        },
        onKeyDown: (e: React.KeyboardEvent) => {
          (childProps.onKeyDown as ((e: React.KeyboardEvent) => void) | undefined)?.(e);
          if (!e.defaultPrevented)
            handleKeyDown(e as unknown as ReactKeyboardEvent<HTMLButtonElement>);
        },
        "aria-expanded": open,
        "aria-haspopup": "menu" as const,
        "aria-controls": open ? menuId : undefined,
        className: cn(childProps.className as string | undefined, className),
      });
    }

    // Fallback for non-element children (text, fragments): keep wrapper div
    const {
      type: _type,
      form: _form,
      formAction: _fa,
      formEncType: _fe,
      formMethod: _fm,
      formNoValidate: _fnv,
      formTarget: _ft,
      onClick: _onClick,
      onKeyDown: _onKeyDown,
      ...divProps
    } = props;
    return (
      <div
        ref={triggerRef as unknown as React.RefObject<HTMLDivElement>}
        role="button"
        tabIndex={0}
        className={cn("inline-flex cursor-pointer items-center justify-center", className)}
        onClick={handleClick}
        onKeyDown={handleKeyDown as unknown as React.KeyboardEventHandler<HTMLDivElement>}
        aria-expanded={open}
        aria-haspopup="menu"
        aria-controls={open ? menuId : undefined}
        {...(divProps as React.HTMLAttributes<HTMLDivElement>)}
      >
        {children}
      </div>
    );
  }

  return (
    <button
      ref={triggerRef}
      type="button"
      className={cn(
        "inline-flex items-center justify-center gap-2 rounded-lg text-sm font-medium",
        "transition-all duration-150",
        "focus:outline-none focus:ring-2 focus:ring-ring focus:ring-offset-2",
        "active:scale-[0.98]",
        variant === "ghost"
          ? "border-0 bg-transparent text-muted-foreground hover:text-foreground hover:bg-accent"
          : "border border-input bg-background px-4 py-2 hover:bg-accent hover:text-accent-foreground hover:border-accent",
        className
      )}
      onClick={handleClick}
      onKeyDown={handleKeyDown}
      aria-expanded={open}
      aria-haspopup="menu"
      aria-controls={open ? menuId : undefined}
      {...props}
    >
      {children}
      {showChevron && (
        <ChevronDown
          className={cn("h-4 w-4 transition-transform duration-200", open && "rotate-180")}
        />
      )}
    </button>
  );
}

interface DropdownContentProps extends HTMLAttributes<HTMLDivElement> {
  align?: "start" | "center" | "end";
  sideOffset?: number;
}

export function DropdownContent({
  className,
  children,
  align = "start",
  sideOffset = 4,
  ...props
}: DropdownContentProps) {
  const {
    open,
    setOpen,
    triggerRef,
    menuId,
    highlightedIndex,
    setHighlightedIndex,
    itemCount,
    setInputModality,
  } = useDropdownContext();
  const localContentRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ top: number; left: number } | null>(null);

  const updatePosition = useCallback(() => {
    if (triggerRef.current && localContentRef.current) {
      const triggerRect = triggerRef.current.getBoundingClientRect();
      const contentRect = localContentRef.current.getBoundingClientRect();

      let left = triggerRect.left;
      if (align === "center") {
        left = triggerRect.left + (triggerRect.width - contentRect.width) / 2;
      } else if (align === "end") {
        left = triggerRect.right - contentRect.width;
      }

      setPosition({
        top: triggerRect.bottom + sideOffset,
        left: Math.max(8, Math.min(left, window.innerWidth - contentRect.width - 8)),
      });
    }
  }, [align, sideOffset, triggerRef]);

  useEffect(() => {
    if (open) {
      // Reset position when opening so we don't flash at old position
      setPosition(null);
      // Wait for render then calculate position
      requestAnimationFrame(updatePosition);
      window.addEventListener("resize", updatePosition);
      window.addEventListener("scroll", updatePosition, true);
    }
    return () => {
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [open, updatePosition]);

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (
        localContentRef.current &&
        !localContentRef.current.contains(e.target as Node) &&
        triggerRef.current &&
        !triggerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
        triggerRef.current?.focus();
      }
    };

    const handleKeyDown = (e: KeyboardEvent) => {
      switch (e.key) {
        case "Escape":
          e.preventDefault();
          setOpen(false);
          triggerRef.current?.focus();
          break;
        case "ArrowDown":
          e.preventDefault();
          setInputModality("keyboard");
          setHighlightedIndex(highlightedIndex < itemCount - 1 ? highlightedIndex + 1 : 0);
          break;
        case "ArrowUp":
          e.preventDefault();
          setInputModality("keyboard");
          setHighlightedIndex(highlightedIndex > 0 ? highlightedIndex - 1 : itemCount - 1);
          break;
        case "Home":
          e.preventDefault();
          setInputModality("keyboard");
          setHighlightedIndex(0);
          break;
        case "End":
          e.preventDefault();
          setInputModality("keyboard");
          setHighlightedIndex(itemCount - 1);
          break;
        case "Tab":
          // Close on tab to allow natural tab flow
          setOpen(false);
          break;
      }
    };

    if (open) {
      document.addEventListener("mousedown", handleClickOutside);
      document.addEventListener("keydown", handleKeyDown);
    }

    return () => {
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleKeyDown);
    };
  }, [
    open,
    setOpen,
    triggerRef,
    highlightedIndex,
    setHighlightedIndex,
    itemCount,
    setInputModality,
  ]);

  if (!open) return null;

  return createPortal(
    <div
      ref={localContentRef}
      id={menuId}
      role="menu"
      aria-orientation="vertical"
      tabIndex={-1}
      className={cn(
        "fixed z-50 min-w-[8rem] overflow-hidden rounded-lg border bg-popover/95 p-1.5 text-popover-foreground shadow-xl backdrop-blur-sm",
        // Only apply animation after position is calculated
        position !== null && "animate-in fade-in-0 zoom-in-95 slide-in-from-top-2",
        "ring-1 ring-black/5 dark:ring-white/10",
        className
      )}
      style={
        position === null
          ? // Position off-screen while measuring dimensions
            { top: -9999, left: -9999, visibility: "hidden" as const }
          : { top: position.top, left: position.left }
      }
      {...props}
    >
      {children}
    </div>,
    document.body
  );
}

interface DropdownItemProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  selected?: boolean;
}

export function DropdownItem({
  className,
  children,
  selected,
  onClick,
  ...props
}: DropdownItemProps) {
  const {
    setOpen,
    triggerRef,
    highlightedIndex,
    registerItem,
    setHighlightedIndex,
    inputModalityRef,
    setInputModality,
  } = useDropdownContext();
  const itemRef = useRef<HTMLButtonElement>(null);
  const [itemIndex, setItemIndex] = useState<number>(-1);

  // Register this item and get its index
  useEffect(() => {
    const index = registerItem();
    setItemIndex(index);
  }, [registerItem]);

  // Focus management when highlighted. Guard on itemIndex >= 0: before
  // registration both itemIndex and highlightedIndex are -1, and without the
  // guard every item would steal focus on mount (the last one winning).
  useEffect(() => {
    if (itemIndex >= 0 && highlightedIndex === itemIndex && itemRef.current) {
      itemRef.current.focus();
    }
  }, [highlightedIndex, itemIndex]);

  const isHighlighted = itemIndex >= 0 && highlightedIndex === itemIndex;

  const handleKeyDown = (e: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onClick?.(e as unknown as React.MouseEvent<HTMLButtonElement>);
      setOpen(false);
      triggerRef.current?.focus();
    }
  };

  return (
    <button
      ref={itemRef}
      type="button"
      role="menuitem"
      tabIndex={isHighlighted ? 0 : -1}
      className={cn(
        "relative flex w-full cursor-pointer select-none items-center rounded-md px-2.5 py-2 text-sm outline-none",
        "transition-colors duration-100",
        "hover:bg-accent hover:text-accent-foreground",
        "focus:bg-accent focus:text-accent-foreground",
        "disabled:pointer-events-none disabled:opacity-50",
        isHighlighted && "bg-accent text-accent-foreground",
        className
      )}
      onClick={(e) => {
        onClick?.(e);
        setOpen(false);
      }}
      onKeyDown={handleKeyDown}
      onMouseMove={() => setInputModality("mouse")}
      onMouseEnter={() => {
        // Only steal focus on hover when the user is actually using the
        // mouse. Without this, an arrow-key navigator would lose their
        // selection any time the cursor happened to be sitting on a
        // different item — a common trigger when the dropdown opens
        // beneath the cursor.
        if (inputModalityRef.current === "mouse") {
          setHighlightedIndex(itemIndex);
        }
      }}
      {...props}
    >
      {selected && <Check className="mr-2 h-4 w-4 text-primary" />}
      {children}
    </button>
  );
}

export function DropdownSeparator({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("-mx-1.5 my-1.5 h-px bg-border/50", className)} {...props} />;
}

export function DropdownLabel({ className, ...props }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        "px-2.5 py-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground",
        className
      )}
      {...props}
    />
  );
}

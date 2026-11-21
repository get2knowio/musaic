import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useLocation } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

import FilterBar from "@/components/FilterBar";
import { useGlobalShortcuts } from "@/hooks/useGlobalShortcuts";
import type { AppConfig } from "@/lib/types";
import { resetUiRequests, useShortcutsDialogOpen } from "@/lib/uiRequests";

const getShortcutsOpen = () => screen.getByTestId("shortcuts").textContent === "open";

function ShortcutsProbe() {
  return <output data-testid="shortcuts">{useShortcutsDialogOpen() ? "open" : "closed"}</output>;
}

function Harness({ withSearch = true }: { withSearch?: boolean }) {
  useGlobalShortcuts();
  const { pathname } = useLocation();
  return (
    <div>
      <ShortcutsProbe />
      <output data-testid="path">{pathname}</output>
      {withSearch && <input type="search" aria-label="Filter" aria-keyshortcuts="/" />}
      <input type="text" aria-label="Other field" />
      <textarea aria-label="Notes" />
      <select aria-label="Pick">
        <option>a</option>
      </select>
      <div contentEditable suppressContentEditableWarning aria-label="Editable" tabIndex={0} />
      <button>plain</button>
    </div>
  );
}

function setup(withSearch = true, at = "/library") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
  qc.setQueryData(["albums-snapshot"], []);
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[at]}>
        <Harness withSearch={withSearch} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function desktop(marker: Record<string, unknown>) {
  Object.defineProperty(window, "__musaicDesktop", { value: Object.freeze(marker), configurable: true });
}

const path = () => screen.getByTestId("path").textContent;

afterEach(() => {
  vi.unstubAllGlobals();
  delete (window as { __musaicDesktop?: unknown }).__musaicDesktop;
  resetUiRequests();
});

describe("global shortcuts", () => {
  it("/ focuses the filter field and does not type a slash into it", () => {
    setup();
    const notPrevented = fireEvent.keyDown(screen.getByText("plain"), { key: "/" });
    expect(notPrevented).toBe(false);
    expect(screen.getByLabelText("Filter")).toHaveFocus();
    expect(screen.getByLabelText("Filter")).toHaveValue("");
  });

  it("/ is left alone when the page has no filter field", () => {
    setup(false);
    const notPrevented = fireEvent.keyDown(screen.getByText("plain"), { key: "/" });
    expect(notPrevented).toBe(true);
  });

  it(", opens Settings", () => {
    setup();
    fireEvent.keyDown(screen.getByText("plain"), { key: "," });
    expect(path()).toBe("/settings");
  });

  it("ignores both keys while typing in a field", () => {
    setup();
    for (const el of [
      screen.getByLabelText("Other field"),
      screen.getByLabelText("Notes"),
      screen.getByLabelText("Pick"),
      screen.getByLabelText("Editable"),
      screen.getByLabelText("Filter"),
    ]) {
      el.focus();
      expect(fireEvent.keyDown(el, { key: "/" })).toBe(true);
      expect(fireEvent.keyDown(el, { key: "," })).toBe(true);
    }
    expect(path()).toBe("/library");
    expect(screen.getByLabelText("Filter")).toHaveFocus();
  });

  it("ignores both keys while a modal dialog is open", () => {
    setup();
    render(<div role="dialog" aria-label="Modal" />);
    expect(fireEvent.keyDown(screen.getByText("plain"), { key: "/" })).toBe(true);
    fireEvent.keyDown(screen.getByText("plain"), { key: "," });
    expect(path()).toBe("/library");
    expect(screen.getByLabelText("Filter")).not.toHaveFocus();
  });

  it("ignores both keys with Ctrl, Cmd or Alt held", () => {
    setup();
    for (const mod of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }]) {
      expect(fireEvent.keyDown(screen.getByText("plain"), { key: "/", ...mod })).toBe(true);
      fireEvent.keyDown(screen.getByText("plain"), { key: ",", ...mod });
    }
    expect(path()).toBe("/library");
    expect(screen.getByLabelText("Filter")).not.toHaveFocus();
  });
});

describe("? and the menu keys", () => {
  it("? opens Keyboard Shortcuts, not while typing", () => {
    setup();
    expect(fireEvent.keyDown(screen.getByLabelText("Notes"), { key: "?" })).toBe(true);
    expect(getShortcutsOpen()).toBe(false);
    fireEvent.keyDown(screen.getByText("plain"), { key: "?" });
    expect(getShortcutsOpen()).toBe(true);
  });

  it("Ctrl-1 / 2 / , / F switch pages in the desktop window without a menu bar (Windows, Linux)", () => {
    desktop({ reveal: true, nativeMenu: false });
    setup(true, "/library");
    fireEvent.keyDown(screen.getByText("plain"), { key: "2", ctrlKey: true });
    expect(path()).toBe("/review");
    fireEvent.keyDown(screen.getByText("plain"), { key: "1", ctrlKey: true });
    expect(path()).toBe("/library");
    // From a text field too, as a menu key would.
    fireEvent.keyDown(screen.getByLabelText("Notes"), { key: ",", ctrlKey: true });
    expect(path()).toBe("/settings");
    fireEvent.keyDown(screen.getByText("plain"), { key: "f", ctrlKey: true });
    expect(path()).toBe("/library");
  });

  it("leaves the menu keys to the native menu (macOS) and to the browser", () => {
    for (const marker of [{ reveal: true, nativeMenu: true }, null]) {
      if (marker) desktop(marker);
      const v = setup(true, "/library");
      expect(fireEvent.keyDown(screen.getByText("plain"), { key: "2", metaKey: true })).toBe(true);
      expect(path()).toBe("/library");
      v.unmount();
      delete (window as { __musaicDesktop?: unknown }).__musaicDesktop;
    }
  });
});

describe("shortcut hint", () => {
  it("the Library search shows / and declares it", () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
    qc.setQueryData(["albums-snapshot"], []);
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 404 })));
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/library"]}>
          <FilterBar totalCount={0} libraryDir={null} scannedAt={null} />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    const search = screen.getByRole("searchbox");
    expect(search).toHaveAttribute("aria-keyshortcuts", "/");
    expect(search.parentElement?.querySelector("kbd")).toHaveTextContent("/");
  });
});

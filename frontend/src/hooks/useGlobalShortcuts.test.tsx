import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, useLocation } from "react-router-dom";

vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn() }) }));

import Titlebar from "@/components/Titlebar";
import { useGlobalShortcuts } from "@/hooks/useGlobalShortcuts";
import type { AppConfig } from "@/lib/types";

function Harness({ withSearch = true }: { withSearch?: boolean }) {
  useGlobalShortcuts();
  const { pathname } = useLocation();
  return (
    <div>
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

function setup(withSearch = true) {
  return render(
    <MemoryRouter initialEntries={["/library"]}>
      <Harness withSearch={withSearch} />
    </MemoryRouter>,
  );
}

const path = () => screen.getByTestId("path").textContent;

afterEach(() => {
  vi.unstubAllGlobals();
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

describe("shortcut hint", () => {
  it("the titlebar search shows / and declares it", () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
    qc.setQueryData(["config"], { library_dir: "/music", exists: true } as AppConfig);
    qc.setQueryData(["ai-providers"], { selected: null, providers: [], uninstalled: [] });
    vi.stubGlobal("fetch", vi.fn(async () => new Response("{}", { status: 404 })));
    render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={["/library"]}>
          <Titlebar />
        </MemoryRouter>
      </QueryClientProvider>,
    );
    const search = screen.getByRole("searchbox");
    expect(search).toHaveAttribute("aria-keyshortcuts", "/");
    expect(search.parentElement?.querySelector("kbd")).toHaveTextContent("/");
  });
});

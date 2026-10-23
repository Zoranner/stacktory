document.querySelectorAll("[data-entry-select]").forEach((select) => {
  const endpointScopes = Array.from(document.querySelectorAll("[data-endpoint-scope]"));
  const endpointDisplay = document.querySelector("[data-entry-url]");

  select.addEventListener("change", () => {
    const previousEndpoint = select.dataset.currentEndpoint;
    const nextEndpoint = select.value;
    if (!previousEndpoint || previousEndpoint === nextEndpoint) return;

    endpointScopes.forEach((scope) => {
      scope.textContent = scope.textContent.split(previousEndpoint).join(nextEndpoint);
    });
    if (endpointDisplay) endpointDisplay.textContent = nextEndpoint;
    select.dataset.currentEndpoint = nextEndpoint;
  });
});

document.querySelectorAll('[role="tab"][aria-controls]').forEach((tab, index, tabs) => {
  const activate = (selected, focus) => {
    tabs.forEach((item) => {
      const panel = document.getElementById(item.getAttribute("aria-controls"));
      const isSelected = item === selected;
      item.setAttribute("aria-selected", String(isSelected));
      item.tabIndex = isSelected ? 0 : -1;
      if (panel) panel.hidden = !isSelected;
    });
    if (focus) selected.focus();
  };

  tab.addEventListener("click", () => activate(tab, false));
  tab.addEventListener("keydown", (event) => {
    let target = null;
    if (event.key === "ArrowRight") target = tabs[(index + 1) % tabs.length];
    if (event.key === "ArrowLeft") target = tabs[(index - 1 + tabs.length) % tabs.length];
    if (event.key === "Home") target = tabs[0];
    if (event.key === "End") target = tabs[tabs.length - 1];
    if (target) {
      event.preventDefault();
      activate(target, true);
    }
  });
});

document.querySelectorAll("[data-copy-target]").forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.getElementById(button.dataset.copyTarget);
    const status = document.getElementById("copy-status");
    if (!target) return;
    try {
      await navigator.clipboard.writeText(target.innerText);
      if (status) status.textContent = "已复制提示词";
    } catch {
      if (status) status.textContent = "复制失败，请手动选择文本";
    }
  });
});

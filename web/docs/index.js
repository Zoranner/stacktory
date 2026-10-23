document.querySelectorAll("[data-entry-select]").forEach((root) => {
  const trigger = root.querySelector("[data-entry-trigger]");
  const listbox = root.querySelector("[data-entry-listbox]");
  const label = root.querySelector("[data-entry-label]");
  const options = Array.from(root.querySelectorAll("[role='option']"));
  const endpointScopes = Array.from(document.querySelectorAll("[data-endpoint-scope]"));
  const endpointDisplay = document.querySelector("[data-entry-url]");
  let activeIndex = Math.max(
    0,
    options.findIndex((option) => option.getAttribute("aria-selected") === "true"),
  );

  const close = (focusTrigger) => {
    listbox.hidden = true;
    trigger.setAttribute("aria-expanded", "false");
    if (focusTrigger) trigger.focus();
  };

  const open = () => {
    listbox.hidden = false;
    trigger.setAttribute("aria-expanded", "true");
    options[activeIndex]?.focus();
  };

  const select = (option) => {
    const previousEndpoint = root.dataset.currentEndpoint;
    const nextEndpoint = option.dataset.endpoint;
    if (!nextEndpoint) return;
    if (previousEndpoint && previousEndpoint !== nextEndpoint) {
      endpointScopes.forEach((scope) => {
        scope.textContent = scope.textContent.split(previousEndpoint).join(nextEndpoint);
      });
    }
    options.forEach((item) => {
      item.setAttribute("aria-selected", String(item === option));
    });
    activeIndex = options.indexOf(option);
    if (label) label.textContent = option.dataset.entryName;
    if (endpointDisplay) endpointDisplay.textContent = nextEndpoint;
    root.dataset.currentEndpoint = nextEndpoint;
    close(true);
  };

  trigger.addEventListener("click", () => {
    if (listbox.hidden) open();
    else close(false);
  });

  trigger.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      open();
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      if (listbox.hidden) open();
      else close(false);
      return;
    }
    if (event.key === "Escape") close(false);
  });

  options.forEach((option, index) => {
    option.addEventListener("click", () => select(option));
    option.addEventListener("keydown", (event) => {
      let nextIndex = null;
      if (event.key === "ArrowDown") nextIndex = (index + 1) % options.length;
      if (event.key === "ArrowUp") nextIndex = (index - 1 + options.length) % options.length;
      if (event.key === "Home") nextIndex = 0;
      if (event.key === "End") nextIndex = options.length - 1;
      if (nextIndex !== null) {
        event.preventDefault();
        activeIndex = nextIndex;
        options[activeIndex].focus();
        return;
      }
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        select(option);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        close(true);
      }
    });
  });

  root.addEventListener("focusout", (event) => {
    if (!root.contains(event.relatedTarget) && !listbox.hidden) close(false);
  });

  document.addEventListener("click", (event) => {
    if (!listbox.hidden && !root.contains(event.target)) close(false);
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

const tabs = Array.from(document.querySelectorAll('[role="tab"][aria-controls]'));

function activateTab(selectedTab, shouldFocus) {
  tabs.forEach((tab) => {
    const selected = tab === selectedTab;
    const panel = document.getElementById(tab.getAttribute("aria-controls"));

    tab.setAttribute("aria-selected", String(selected));
    tab.tabIndex = selected ? 0 : -1;
    if (panel) {
      panel.hidden = !selected;
    }
  });

  if (shouldFocus) {
    selectedTab.focus();
  }
}

tabs.forEach((tab, index) => {
  tab.addEventListener("click", () => activateTab(tab, false));
  tab.addEventListener("keydown", (event) => {
    let targetIndex = null;
    if (event.key === "ArrowRight") {
      targetIndex = (index + 1) % tabs.length;
    } else if (event.key === "ArrowLeft") {
      targetIndex = (index - 1 + tabs.length) % tabs.length;
    } else if (event.key === "Home") {
      targetIndex = 0;
    } else if (event.key === "End") {
      targetIndex = tabs.length - 1;
    }

    if (targetIndex !== null) {
      event.preventDefault();
      activateTab(tabs[targetIndex], true);
    }
  });
});

const copyButtons = document.querySelectorAll("[data-copy-target]");
const copyStatus = document.getElementById("copy-status");

copyButtons.forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.getElementById(button.dataset.copyTarget);
    if (!target) {
      return;
    }

    const text = target.innerText;
    try {
      await navigator.clipboard.writeText(text);
      if (copyStatus) {
        copyStatus.textContent = "已复制提示词";
      }
      const label = button.querySelector(".button-label");
      if (label) {
        label.textContent = "已复制";
        window.setTimeout(() => {
          label.textContent = "复制提示词";
        }, 1800);
      }
    } catch {
      if (copyStatus) {
        copyStatus.textContent = "复制失败，请手动选择文本";
      }
    }
  });
});

// Each chart owns its tooltip. Saved text is displayed as text, not HTML.
for (const figure of document.querySelectorAll('.pie-figure')) {
  const tooltip = figure.querySelector('.pie-tooltip');
  if (!tooltip) continue;
  const pie = figure.querySelector('.pie');
  let pinned = null;

  function show(slice, event) {
    tooltip.textContent = slice.dataset.pieDetail;
    tooltip.hidden = false;
    const frame = figure.getBoundingClientRect();
    const chart = pie.getBoundingClientRect();
    const x = event ? event.clientX - frame.left : chart.left - frame.left + chart.width / 2;
    const y = event ? event.clientY - frame.top : chart.top - frame.top + chart.height / 2;
    const left = Math.max(0, Math.min(x + 12, frame.width - tooltip.offsetWidth));
    const top = Math.max(chart.top - frame.top, Math.min(y + 16, chart.bottom - frame.top - tooltip.offsetHeight));
    tooltip.style.left = `${left}px`;
    tooltip.style.top = `${top}px`;
  }

  for (const slice of pie.querySelectorAll('.pie-slice')) {
    // The title is a fallback when scripts are disabled.
    slice.querySelector('title')?.remove();
    slice.addEventListener('pointerenter', event => {
      if (!pinned && event.pointerType !== 'touch') show(slice, event);
    });
    slice.addEventListener('pointermove', event => {
      if (!pinned && event.pointerType !== 'touch') show(slice, event);
    });
    slice.addEventListener('pointerleave', () => {
      if (!pinned && document.activeElement !== slice) tooltip.hidden = true;
    });
    slice.addEventListener('focus', () => show(slice));
    slice.addEventListener('blur', () => {
      if (!pinned) tooltip.hidden = true;
    });
    slice.addEventListener('click', () => {
      pinned = pinned === slice ? null : slice;
      if (pinned) show(slice);
      else tooltip.hidden = true;
    });
    slice.addEventListener('keydown', event => {
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        slice.dispatchEvent(new MouseEvent('click'));
      }
    });
  }

  document.addEventListener('keydown', event => {
    if (event.key === 'Escape') {
      pinned = null;
      tooltip.hidden = true;
    }
  });
  document.addEventListener('pointerdown', event => {
    if (!pie.contains(event.target)) {
      pinned = null;
      tooltip.hidden = true;
    }
  });
}

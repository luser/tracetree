/*global fetch, FileReader */

function elapsed_secs(ms) {
  return (ms / 1000).toFixed(3) + 's';
}

function process_to_rows(data) {
  var cmd = data.cmdline.length > 0 ? data.cmdline[0].split('/').pop() : '<unknown>';
  var start = new Date(data.started);
  var end = new Date(data.ended);
  var rows = [{'pid': data.pid,
               'cmd': cmd,
               'cmdline': data.cmdline.join(' '),
               'start': start,
               'end': end,
               'elapsed': elapsed_secs(end.getTime() - start.getTime())
              }];
  for (var child of data.children) {
    rows.push(...process_to_rows(child));
  }
  return rows;
}

function make_handler(data) {
  return (ev) => {
    console.log(`clicked ${data.pid}`);
    for (var prop of Object.getOwnPropertyNames(data)) {
      var el = document.getElementById('panel-' + prop);
      if (el == null)
        continue;
      el.innerText = data[prop].toString();
    }
    var panel = document.getElementById('panel');
    panel.style.left = ev.pageX + 'px';
    panel.style.top = ev.pageY + 'px';
    panel.style.display = 'block';
    ev.stopPropagation();
  };
}

function draw_chart(rows) {
  console.log(`${rows.length} rows`);
  // Figure out how much space we have to work with.
  var available = parseInt(window.getComputedStyle(document.getElementById('execution')).width.slice(0, -2));
  var start = rows[0].start.getTime();
  var total = rows[0].end.getTime() - start;
  //TODO: allow scrolling for really long profiles.
  var factor = available / total;
  console.log(`available: ${available}, total: ${total}, factor: ${factor}`);
  function scalemin(val) {
    return Math.max(4, scale(val));
  }
  function scale(val) {
    return val * factor;
  }
  rows.sort((a, b) => a.start.getTime() - b.start.getTime());
  var table = document.getElementById('chart');
  var tb = table.tBodies[0];
  while (tb.rows.length > 0) {
    tb.rows[0].remove();
  }

  for (var r of rows) {
    var tr = tb.insertRow();
    var c = tr.insertCell();
    c.className = 'label';
    c.innerText = r.cmd;
    c = tr.insertCell();
    r.startpretty = elapsed_secs(r.start.getTime() - start);
    r.endpretty = elapsed_secs(r.end.getTime() - start);
    var offset = scale(r.start.getTime() - start);
    var length = scalemin(r.end.getTime() - r.start.getTime());
    c.innerHTML = `<div class="bar" style="left: ${offset}px; width: ${length}px" title="${r.elapsed}s"></div>`;
    c.firstChild.addEventListener('click', make_handler(r));
  }
}

function chart_file() {
  console.log('chart_file');
  var file = document.getElementById('input').files[0];
  var reader = new FileReader();
  reader.onload = () => {
    console.log('loaded');
    var json = JSON.parse(reader.result);
    var rows = process_to_rows(json);
    draw_chart(rows);
  };
  reader.readAsText(file);
}

function load_demo() {
  fetch('cargo-sccache-build.json')
    .then((res) => res.json())
    .then((data) => draw_chart(process_to_rows(data)));
}

window.addEventListener('DOMContentLoaded', () => {
  document.getElementById('input').onchange = chart_file;
  document.getElementById('demo').onclick = load_demo;
  document.body.addEventListener('click', () => {
    console.log('clicked body');
    document.getElementById('panel').style.display = 'none';
  });
});

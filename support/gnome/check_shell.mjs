import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import St from 'gi://St';
import Clutter from 'gi://Clutter';
import Pango from 'gi://Pango';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Scripting from 'resource:///org/gnome/shell/ui/scripting.js';
const pause = ms => new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {resolve(); return GLib.SOURCE_REMOVE;}));
function check(ok, text) { if (!ok) throw new Error(text); }

export async function run() {
    // Correctness checks can outlive the perf helper's idle timeout. The
    // isolated Wayland/DBus session owns helper cleanup, not its final Exit RPC.
    await Scripting.disableHelperAutoExit();
    await pause(1200);
    let ext = Main.extensionManager.lookup('localsend-transfer@local');
    if (!ext) {
        const extensionDir = Gio.File.new_for_uri(import.meta.url).get_parent().get_child('localsend-transfer@local');
        ext=Main.extensionManager.createExtensionObject('localsend-transfer@local',extensionDir,2);
        await Main.extensionManager.loadExtension(ext);
        await Main.extensionManager._callExtensionEnable(ext.uuid);
    }
    check(ext?.stateObj, `extension not loaded: ${JSON.stringify(ext?.errors)}`);
    const e=ext.stateObj;
    const base={id:'test',alias:'7的iphone',phase:'running',bytes:68,total_bytes:100,
        paths:['/tmp/中文 空格 (2).txt'],file_count:1,destination:GLib.build_filenamev([GLib.get_home_dir(),'LocalSend']),remaining_ms:null,detail:''};
    const vitals=new St.Label({text:'▥ 46.1%  ♨ 43.8°C  ↓ 16.3 KB/s  ◉ 2540 RPM',y_align:Clutter.ActorAlign.CENTER});
    Main.panel._leftBox.insert_child_at_index(vitals,1);
    // Stop live polling in the isolated desktop for deterministic snapshots.
    e.enabled=false;
    if(e.timer) GLib.Source.remove(e.timer);
    e.timer=0;
    const panel=e.transfer;
    const originalRow=panel.indicator.container;
    const receive=transfer=>e.render({receive:transfer,send:null});
    const send=transfer=>e.render({receive:null,send:transfer});
    const onlyOnePanel=()=>{
        check(Object.keys(Main.panel.statusArea).filter(name=>name.startsWith('localsend-')).length===1,'exactly one registered transfer panel');
        check(e.transfer.indicator.container===originalRow,'same row reused');
    };
    receive(base);
    await pause(700);
    onlyOnePanel();
    check(panel.indicator.container.visible && !panel.open.opacity && !panel.open.reactive,'receiving state');
    check(panel.peer.text === base.alias && panel.peer.clutter_text.ellipsize === Pango.EllipsizeMode.NONE,
        'device name remains complete without ellipsis');
    receive({...base,alias:'完整设备名称——这是一台接收文件的电脑'});
    await pause(150);
    const [,naturalNameWidth] = panel.peer.get_preferred_width(-1);
    check(panel.peer.width >= naturalNameWidth, 'long device name has its full natural width');
    receive(base);
    await pause(150);
    const expectedFont=Main.panel.get_theme_node().get_font();
    for (const widget of [panel.peer]) {
        const actual=widget.get_theme_node().get_font();
        check(actual.equal(expectedFont), `font mismatch: ${actual.to_string()} vs ${expectedFont.to_string()}`);
    }
    print('PANEL FONT: '+expectedFont.to_string());
    const [,textY]=panel.peer.get_transformed_position();
    const [,vitalsY]=vitals.get_transformed_position();
    check(Math.abs(textY-vitalsY)<=1,'text aligns with panel baseline');
    check(!panel.row.get_children().some(child=>child instanceof St.Button && child.get_label()==='打开文件夹'),'folder action removed');
    const visibleActors = expected => {
        const actual = [panel.peerButton,panel.accept,panel.decline,panel.progress,panel.open,panel.sent]
            .filter(child => child.get_paint_opacity() > 0);
        check(actual.length === expected.length && actual.every((child, i) => child === expected[i]),
            'only device, progress and applicable actions visible');
    };
    visibleActors([panel.peerButton,panel.progress]);
    check(!panel.left && !panel.percent && !panel.result, 'status and percent actors removed');
    const screenshot = new Shell.Screenshot();
    async function capture(name) {
        const stream=Gio.File.new_for_path(GLib.getenv('LS_UI_OUTPUT')+'/'+name+'.png').replace(null,false,Gio.FileCreateFlags.REPLACE_DESTINATION,null);
        await screenshot.screenshot(false,stream);stream.close(null);
    }
    await capture('panel-running');
    const geometry={};
    for (const percent of [0,25,50,75,100]) {
        receive({...base,bytes:percent});
        await pause(650);
        const [x,y]=panel.progress.get_transformed_position();
        const [width,height]=panel.progress.get_transformed_size();
        geometry[percent]={x,y,width,height};
        await capture('panel-progress-'+percent);
    }
    for (const [name,phase,bytes] of [
        ['failed-50','failed',50],['cancelled-50','cancelled',50],
        ['failed-0','failed',0],['cancelled-0','cancelled',0],['success','success',50],
    ]) {
        receive({...base,id:name,phase,bytes,remaining_ms:4500,detail:'测试失败原因'});
        await pause(650);
        check(panel.current.fraction === (phase === 'success' ? 1 : bytes / 100), 'terminal fraction retained');
        check(panel.open.reactive === (phase === 'success'), 'open enabled only after success');
        visibleActors(phase === 'success' ? [panel.peerButton,panel.progress,panel.open] : [panel.peerButton,panel.progress,panel.sent]);
        if (phase !== 'success') check(panel.sent.text === (phase === 'cancelled' ? '传输取消' : '传输失败'), 'explicit error result');
        if (phase === 'success')
            check(panel.open.get_theme_node().get_font().equal(expectedFont), 'open uses panel font');
        const [x,y]=panel.progress.get_transformed_position();
        const [width,height]=panel.progress.get_transformed_size();
        geometry[name]={x,y,width,height};
        await capture('panel-'+name);
    }
    const details=panel.indicator.menu._getMenuItems().map(item=>item.label?.text).filter(Boolean);
    check(details.includes('测试失败原因') && details.includes('保存位置：'+base.destination), 'details retain error and destination');
    Gio.File.new_for_path(GLib.getenv('LS_UI_OUTPUT')+'/panel-progress-geometry.json').replace_contents(
        JSON.stringify(geometry),null,false,Gio.FileCreateFlags.REPLACE_DESTINATION,null);
    receive({...base,phase:'pending'});
    visibleActors([panel.peerButton,panel.accept,panel.decline]);
    const clicks=[];
    e.enabled=true;
    e.schedule=()=>{};
    e.request=async message=>{
        clicks.push(message);
        return {ok:true,receive:message.accept ? base : null,send:null};
    };
    const pointer=global.stage.context.get_backend().get_default_seat().create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
    async function click(widget) {
        await pause(650);
        const [x,y]=widget.get_transformed_position();
        const [width,height]=widget.get_transformed_size();
        print(`CLICK target=${widget} rect=${x},${y},${width},${height} reactive=${widget.reactive} outerGesture=${panel.indicator._clickGesture.enabled}`);
        pointer.notify_absolute_motion(GLib.get_monotonic_time(),x+width/2,y+height/2);
        await pause(50);
        pointer.notify_button(GLib.get_monotonic_time(),Clutter.BUTTON_PRIMARY,Clutter.ButtonState.PRESSED);
        await pause(50);
        pointer.notify_button(GLib.get_monotonic_time(),Clutter.BUTTON_PRIMARY,Clutter.ButtonState.RELEASED);
        await pause(100);
    }
    await click(panel.accept);
    check(clicks.length===1 && clicks[0].command==='answer_transfer' && clicks[0].session_id===base.id && clicks[0].accept,
        'actual pointer accept sends the current session decision: '+JSON.stringify(clicks));
    check(panel.current.phase==='running' && panel.transferBox.opacity===255 && !panel.decisionBox.opacity, 'accept switches to progress');
    receive({...base,phase:'pending'});
    await click(panel.decline);
    check(clicks.length===2 && !clicks[1].accept,'actual pointer decline sends rejection');
    check(!panel.indicator.container.visible, 'decline immediately hides the panel');
    check(!panel.indicator.menu.isOpen,'decision clicks do not open details');
    receive({...base,phase:'pending'});
    await click(panel.peerButton);
    check(panel.indicator.menu.isOpen, 'device name still opens details');
    await click(panel.peerButton);
    check(!panel.indicator.menu.isOpen, 'device name closes details');
    e.enabled=false;
    await pause(200);await capture('panel-pending');
    receive({...base,phase:'success',remaining_ms:4999});
    await pause(650);
    check(panel.open.reactive,'successful open action');
    const launches=[];e.launch=path=>launches.push(path);await click(panel.open);
    check(launches[0]===base.paths[0],'actual file path');
    const paths=['/tmp/z-first-received (1).txt','/tmp/a-second-received.txt'];
    receive({...base,phase:'success',remaining_ms:4999,paths,file_count:2});
    onlyOnePanel();
    const before=launches.length;await click(panel.open);
    check(launches.length===before+1 && launches.at(-1)===paths[0],'opens just the first batch file');
    receive({...base,phase:'success',remaining_ms:4999,paths:['/tmp/LocalSend-文本.txt'],file_count:1});
    await click(panel.open);
    check(launches.at(-1)==='/tmp/LocalSend-文本.txt', 'received text opens the saved text document');
    await capture('panel-text-success');
    send({...base,id:'send-test',alias:'测试电脑',bytes:45});
    await pause(200);
    onlyOnePanel();
    check(panel.peer.text==='测试电脑','same panel switches to sending');
    visibleActors([panel.peerButton,panel.progress]);
    await capture('panel-sending');
    for (const phase of ['success','failed','cancelled']) {
        send({...base,phase,remaining_ms:4000});
        visibleActors([panel.peerButton,panel.progress,panel.sent]);
        if (phase === 'success') {
            check(panel.sent.text === '传输成功', 'successful sending text');
            check(panel.sent.get_theme_node().get_font().equal(expectedFont), 'success text uses system font');
            await pause(150);await capture('panel-send-success');
        }
        check(panel.current.fraction === (phase === 'success' ? 1 : 0.68), 'send result fraction');
        check(panel.current.isError === (phase !== 'success'), 'send error color state');
    }
    receive(null);check(!panel.indicator.container.visible,'decline or idle hides the panel');
    send({...base,phase:'success',remaining_ms:0});check(!panel.indicator.container.visible,'expired result hidden');
    // A fresh ID gets one continuous entrance; fast completion never resets it.
    receive({...base,id:'fast',bytes:2});
    const entranceStart=panel.motion.started;
    await pause(150);
    receive({...base,id:'fast',phase:'success',remaining_ms:4999});
    check(panel.motion.started===entranceStart,'fast completion preserves entrance clock');
    check(panel.frame.fraction===1 && !panel.open.reactive,'real success immediate, transparent action not clickable');
    await pause(700);
    check(panel.frame.reveal===1 && panel.frame.resultReveal===1 && panel.open.reactive,'uniform entrance completes');
    const stable=()=>[panel.row.width,panel.body.width,panel.progress.get_transformed_position()[0]];
    const completedGeometry=stable();
    receive({...base,id:'fast',phase:'running',bytes:80});
    await pause(100);
    check(JSON.stringify(stable())===JSON.stringify(completedGeometry),'result appearance cannot move progress or resize row');
    receive({...base,id:'fast',phase:'pending'});
    await pause(100);
    check(JSON.stringify(stable())===JSON.stringify(completedGeometry),'decision-to-progress keeps layout stable');
    receive({...base,id:'expiry',phase:'success',remaining_ms:1200});
    await pause(950);
    check(panel.frame.reveal>0 && panel.frame.reveal<1,'native soft edge fades before expiry');
    await capture('panel-motion-exit');
    await pause(300);
    check(!panel.indicator.container.visible && !panel.animationTimer,'deadline hides and stops animation even without polling');
    receive({...base,id:'old',phase:'success',remaining_ms:300});
    receive({...base,id:'new',bytes:20});
    await pause(700);
    check(panel.indicator.container.visible && panel.current.id==='new','old result cannot hide a new transfer');
    const preferences=new Gio.Settings({schema_id:'org.gnome.desktop.interface'});
    const animationsBefore=preferences.get_boolean('enable-animations');
    preferences.set_boolean('enable-animations',false);
    receive({...base,id:'reduced',phase:'success',remaining_ms:5000});
    await pause(100);
    check(panel.frame.reveal===1 && panel.frame.resultReveal===1 && panel.open.reactive,
        'system animation switch updates native effects and buttons immediately');
    preferences.set_boolean('enable-animations',animationsBefore);
    await pause(100);
    receive({...base,id:'cleanup',phase:'success',remaining_ms:5000});
    e.disable();
    check(!panel.animationTimer,'disable cancels animation timer');
    check(!Main.panel.statusArea['localsend-transfer'],'disable removes the only transfer panel');
    print('PASS: one reused panel for receive/send, batch first file, fonts, baseline, progress, decision buttons, failures and expiry');
}

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import Shell from 'gi://Shell';
import St from 'gi://St';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as Scripting from 'resource:///org/gnome/shell/ui/scripting.js';
const pause = ms => new Promise(resolve => GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {resolve(); return GLib.SOURCE_REMOVE;}));
let checks=0;
function check(ok, message) { if (!ok) throw new Error(message); checks++; }
export async function run() {
    await Scripting.disableHelperAutoExit();
    await pause(1000);
    const dir=Gio.File.new_for_uri(import.meta.url).get_parent().get_child('localsend-transfer@local');
    let ext=Main.extensionManager.lookup('localsend-transfer@local');
    if(!ext) {ext=Main.extensionManager.createExtensionObject('localsend-transfer@local',dir,2); await Main.extensionManager.loadExtension(ext); await Main.extensionManager._callExtensionEnable(ext.uuid);}
    check(ext?.stateObj,`extension load: ${ext?.errors}`);
    const e=ext.stateObj; e.enabled=false; e.generation++;
    if(e.timer) GLib.Source.remove(e.timer); e.timer=0; e.monitor?.cancel();
    await pause(100);
    e.enabled=true; e.schedule=()=>{};
    Main.overview.hide();
    const panel=e.transfer, requests=[];
    // Put the source away from the monitor edge, as with the user's Vitals
    // widgets, so edge clamping cannot mask a popup-alignment regression.
    const spacer=new St.Widget({width:360,height:Main.panel.height});
    Main.panel._leftBox.insert_child_at_index(spacer,Main.panel._leftBox.get_children().indexOf(panel.indicator.container));
    const t={id:'receive-one',alias:'真实收发设备',phase:'running',bytes:50,total_bytes:100,remaining_ms:null};
    const s=(transfer=null,direction='receive')=>({ok:true,running:true,pid:42,[direction]:transfer});
    e.lifecycle=null;
    e.request=async message=>{requests.push(message); return s(message.command==='cancel_transfer'?{...t,phase:'cancelled',remaining_ms:5000}:message.accept?t:null);};
    const opened=[], actualLaunch=e.launch.bind(e);
    e.launch=path=>opened.push(path);
    let started=0,stopped=0,picked=0;
    e.start=()=>started++; e.stop=()=>stopped++; e.chooseFiles=()=>picked++;
    const pointer=global.stage.context.get_backend().get_default_seat().create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
    async function click(actor,button=Clutter.BUTTON_PRIMARY) {
        await pause(150);
        const [x,y]=actor.get_transformed_position(); const [w,h]=actor.get_transformed_size();
        pointer.notify_absolute_motion(GLib.get_monotonic_time(),x+w/2,y+h/2);
        pointer.notify_button(GLib.get_monotonic_time(),button,Clutter.ButtonState.PRESSED);
        await pause(40);
        pointer.notify_button(GLib.get_monotonic_time(),button,Clutter.ButtonState.RELEASED);
        await pause(100);
    }
    const shot=new Shell.Screenshot();
    async function capture(name) {
        const path=GLib.getenv('LS_UI_OUTPUT')+'/'+name+'.png';
        const stream=Gio.File.new_for_path(path).replace(null,false,Gio.FileCreateFlags.REPLACE_DESTINATION,null);
        await shot.screenshot(false,stream); stream.close(null);
    }
    await pause(400);
    e.render(null); await pause(100);
    check(panel.indicator.container.visible&&panel.motion.phase==='off','white ring remains visible while offline');
    await click(panel.ring); check(started===1,'actual left click starts backend');
    e.render(s()); await pause(200); await capture('ring-green');
    check(panel.motion.phase==='idle'&&!panel.animationTimer,'green idle has no animation loop');
    check(panel.drawing.width>0&&panel.drawing.width<=30,'native panel-sized ring');
    await click(panel.ring);
    check(picked===1&&!panel.indicator.menu.isOpen,'left green directly opens picker without send menu');
    await click(panel.ring,Clutter.BUTTON_SECONDARY); await pause(200);
    check(panel.indicator.menu.isOpen&&panel.indicator.menu._getMenuItems()[0].label.text==='退出','right green opens only exit');
    const center=actor=>actor.get_transformed_position()[0]+actor.get_transformed_size()[0]/2;
    const alignment={ringCenter:center(panel.ring),menuCenter:center(panel.indicator.menu.actor)};
    check(Math.abs(alignment.ringCenter-alignment.menuCenter)<=1.5,'exit popup centers on ring: '+JSON.stringify(alignment));
    print('RING POPUP ALIGNMENT: '+JSON.stringify(alignment));
    await capture('ring-exit-centered');
    panel.indicator.menu.close(); await pause(200);
    await click(panel.ring);
    check(picked===2&&!panel.indicator.menu.isOpen,'left green directly opens picker after context menu closes');
    e.render(s({...t,phase:'pending'}));
    check(panel.decision.visible&&panel.motion.phase==='idle','pending keeps green and shows decisions');
    await click(panel.decision.get_children()[2]);
    check(requests.at(-1).command==='answer_transfer'&&!requests.at(-1).accept,'actual decline targets request');
    check(panel.motion.phase==='idle'&&!panel.decision.visible,'decline leaves green ring unchanged');
    e.render(s({...t,phase:'pending'})); await click(panel.decision.get_children()[1]);
    check(requests.at(-1).accept&&panel.motion.phase==='transfer','actual accept starts receive progress');
    await pause(200); await capture('ring-receive-half');
    check(panel.frame.layers.at(-1).color==='purple'&&panel.frame.fraction===.5,'real receive bytes drive purple arc');
    await click(panel.ring);
    check(requests.at(-1).command==='cancel_transfer'&&requests.at(-1).session_id===t.id,'actual ring click cancels only selected session');
    check(panel.motion.phase==='error'&&panel.motion.fraction===.5,'cancel freezes progress and turns red');
    await capture('ring-cancel-red');
    await click(panel.ring); await pause(750);
    check(panel.motion.phase==='idle','red click recovers without stopping backend');
    check(opened.length===0,'pending, transfer, cancellation and error clicks never open files');
    const paths=['/tmp/z-首先收到 (2).txt','/tmp/a-后来收到.txt'];
    for(const direction of ['receive','send']) {
        const result={...t,id:'success-'+direction,phase:'success',remaining_ms:5000,paths,file_count:2};
        e.render(s(result,direction));
        check(panel.motion.phase==='success'&&panel.frame.fraction<.02&&panel.animationTimer,
            direction+' small success starts animated from green even without a running snapshot');
        await pause(180);
        check(panel.frame.fraction>.02&&panel.frame.fraction<1,
            direction+' actual frame timer draws a partial clockwise success arc');
        await capture('ring-'+direction+'-small-midway');
        const visible=panel.frame.fraction;
        e.render(s({...result,remaining_ms:4800},direction));
        check(panel.frame.fraction>=visible&&panel.frame.fraction<1,
            direction+' repeated successful poll preserves the visible completion arc');
        const before=requests.length; await click(panel.ring);
        check(panel.motion.phase==='success'&&requests.length===before,'success click does not alter transfer or result deadline');
        check(opened.length===1&&opened[0]===paths[0],direction==='receive'
            ?'receive success opens only the first final saved path':'send success does not open any files');
        check(!panel.open,'no additional open button');
        await pause(650);
        check(panel.frame.fraction===1&&panel.motion.phase==='success'&&panel.frame.next>1000,
            direction+' completion reaches full color then stops the frame loop during the hold');
        await capture('ring-'+direction+'-success');
    }
    e.render(s({...t,id:'missing-path',phase:'success',remaining_ms:5000,paths:[]}));
    await click(panel.ring); check(opened.length===1,'receive result without a saved path is not opened');
    e.render(s({...t,id:'expired',phase:'success',remaining_ms:5000,paths}));
    panel.motion.deadline=GLib.get_monotonic_time()/1000-1;
    await click(panel.ring); check(opened.length===1,'expired purple result cannot open a file');
    // Exercise the real GIO/default-application route with associations confined
    // to this test session's temporary XDG directories, never the user's defaults.
    const runtime=GLib.get_user_runtime_dir();
    const marker=runtime+'/opened-path'; const opener=runtime+'/open-check.py';
    const received=runtime+'/收到的 中文 $(literal) (2).txt';
    const write=(path,text)=>Gio.File.new_for_path(path).replace_contents(text,null,false,Gio.FileCreateFlags.REPLACE_DESTINATION,null);
    write(received,'LocalSend received content\n');
    write(opener,`import pathlib, sys\npathlib.Path(${JSON.stringify(marker)}).write_text(sys.argv[1])\n`);
    const appDir=GLib.get_user_data_dir()+'/applications'; GLib.mkdir_with_parents(appDir,0o700);
    const desktop=appDir+'/localsend-ring-open-check.desktop';
    write(desktop,`[Desktop Entry]\nType=Application\nName=LocalSend isolated open check\nExec=/usr/bin/python3 ${opener} %f\nMimeType=text/plain;\nNoDisplay=true\n`);
    check(Gio.DesktopAppInfo.new_from_filename(desktop).set_as_default_for_type('text/plain'),'temporary test default handler registered');
    e.launch=actualLaunch;
    e.render(s({...t,id:'real-open',phase:'success',remaining_ms:5000,paths:[received],file_count:1}));
    await click(panel.ring);
    const markerFile=Gio.File.new_for_path(marker);
    for(let i=0;i<20&&!markerFile.query_exists(null);i++) await pause(100);
    check(markerFile.query_exists(null),'actual pointer click launches the system default application');
    const [,contents]=markerFile.load_contents(null);
    check(new TextDecoder().decode(contents)===received,'actual default application gets exact Unicode path as one argument');
    await click(panel.ring,Clutter.BUTTON_SECONDARY);
    check(panel.indicator.menu.isOpen&&panel.indicator.menu._getMenuItems()[0].label.text==='退出','actual right click offers exit');
    await click(panel.indicator.menu._getMenuItems()[0]); check(stopped===1,'exit menu invokes shutdown');
    const before=e.transfer; e.disable();
    spacer.destroy();
    check(!before.animationTimer&&!Main.panel.statusArea['localsend-transfer'],'disable cleans timers and only indicator');
    print(`PASS: ${checks} native GNOME ring, pointer, menu, confirmation, color and cleanup checks`);
}

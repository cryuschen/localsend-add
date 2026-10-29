import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Clutter from 'gi://Clutter';
import Shell from 'gi://Shell';
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
    const t={id:'receive-one',alias:'真实收发设备',phase:'running',bytes:50,total_bytes:100,remaining_ms:null};
    const s=(transfer=null,direction='receive')=>({ok:true,running:true,pid:42,[direction]:transfer});
    e.lifecycle=null;
    e.request=async message=>{requests.push(message); return s(message.command==='cancel_transfer'?{...t,phase:'cancelled',remaining_ms:5000}:message.accept?t:null);};
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
    check(panel.indicator.menu.isOpen&&panel.indicator.menu._getMenuItems()[0].label.text==='发送','left green opens send menu');
    await click(panel.indicator.menu._getMenuItems()[0]); check(picked===1,'send action opens picker');
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
    for(const direction of ['receive','send']) {
        e.render(s({...t,id:'success-'+direction,phase:'success',remaining_ms:5000},direction));
        const before=requests.length; await click(panel.ring);
        check(panel.motion.phase==='success'&&requests.length===before,'success click has no operation');
        check(!panel.open,'file-open action removed');
        await capture('ring-'+direction+'-success');
    }
    await click(panel.ring,Clutter.BUTTON_SECONDARY);
    check(panel.indicator.menu.isOpen&&panel.indicator.menu._getMenuItems()[0].label.text==='退出','actual right click offers exit');
    await click(panel.indicator.menu._getMenuItems()[0]); check(stopped===1,'exit menu invokes shutdown');
    const before=e.transfer; e.disable();
    check(!before.animationTimer&&!Main.panel.statusArea['localsend-transfer'],'disable cleans timers and only indicator');
    print(`PASS: ${checks} native GNOME ring, pointer, menu, confirmation, color and cleanup checks`);
}

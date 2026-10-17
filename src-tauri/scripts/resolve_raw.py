"""DeProxy's optional Resolve bridge. No Canon libraries are redistributed."""
import fcntl
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

MODULES = '/Library/Application Support/Blackmagic Design/DaVinci Resolve/Developer/Scripting/Modules'


def aspect(value):
    if str(value).lower() in ('square', '1:1'):
        return 1.0
    try:
        result = float(value)
    except (ValueError, TypeError):
        raise ValueError('Unsupported Resolve pixel aspect ratio: ' + str(value))
    if not math.isfinite(result) or not 1 <= result <= 2:
        raise ValueError('Unsupported Resolve pixel aspect ratio: ' + str(value))
    return result


def dimensions(width, height, squeeze, output_height):
    """Bound anamorphic RAW output to the selected DCI size; keep the entire frame."""
    w = round(width / height * squeeze * output_height / 2) * 2
    cap = round(output_height * 4096 / 2160 / 2) * 2
    if w > cap:
        return cap, max(2, round(output_height * cap / w / 2) * 2)
    return max(2, w), output_height


def connection():
    sys.path.insert(0, MODULES)
    try:
        import DaVinciResolveScript
        resolve = DaVinciResolveScript.scriptapp('Resolve')
    except Exception as error:
        raise RuntimeError('Install Python 3 and Resolve Studio, open Resolve, and enable local external scripting. ' + str(error))
    if not resolve or 'Studio' not in resolve.GetProductName():
        raise RuntimeError('Open DaVinci Resolve Studio and enable Preferences > System > General > External scripting > Local.')
    send(connected=True)
    return resolve


def clips(folder):
    yield from folder.GetClipList()
    for child in folder.GetSubFolderList():
        yield from clips(child)


def inspect(request):
    resolve = connection()
    project = resolve.GetProjectManager().GetCurrentProject()
    if not project:
        raise RuntimeError('Open a Resolve project first. Its clip attributes can be used for mixed anamorphic footage.')
    if project.IsRenderingInProgress():
        raise RuntimeError('Wait for the current Resolve render to finish.')
    source_files = sorted(p for p in Path(request['sourceDir']).iterdir() if p.is_file() and p.suffix.lower() == '.crm')
    source_paths = {os.path.abspath(p) for p in source_files}
    source_names = {p.name for p in source_files}
    ratios = {}
    if request.get('useProjectAspect', True):
        for clip in clips(project.GetMediaPool().GetRootFolder()):
            path = clip.GetClipProperty('File Path')
            if path and Path(path).name in source_names and os.path.abspath(path) in source_paths:
                key = os.path.abspath(path)
                value = clip.GetClipProperty('PAR')
                ratio = aspect(value) if value else None
                if key in ratios and ratios[key] != ratio:
                    raise RuntimeError('Conflicting aspect settings for ' + Path(path).name)
                ratios[key] = ratio
    plan = []
    for path in source_files:
        if path.is_file() and path.suffix.lower() == '.crm':
            ratio = ratios.get(os.path.abspath(path))
            plan.append({'file': str(path), 'squeeze': ratio if ratio is not None else request['squeeze'], 'fromProject': ratio is not None})
    return {'project': project.GetName(), 'clips': plan}


def cancelled(request):
    return Path(request['cancelPath']).exists()


def check_cancel(request):
    if cancelled(request):
        raise RuntimeError('Cancelled')


def send(**value):
    print(json.dumps(value), flush=True)


def run_tool(args, request):
    with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
        child = subprocess.Popen(args, stdout=output, stderr=errors)
        try:
            while child.poll() is None:
                check_cancel(request)
                time.sleep(.1)
            if child.returncode:
                errors.seek(0)
                raise RuntimeError(errors.read().decode(errors='replace')[-4000:])
            output.seek(0)
            return output.read().decode()
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()


def probe(path, request, count=False):
    return json.loads(run_tool([request['ffprobe'], '-v', 'error', '-threads', '0'] + (['-count_frames'] if count else []) + ['-show_streams', '-show_format', '-of', 'json', str(path)], request))


def verify(request):
    result = probe(request['output'], request, True)
    video = next(s for s in result['streams'] if s['codec_type'] == 'video')
    if int(video['nb_read_frames']) != request['frames']:
        raise RuntimeError('Frame count mismatch; proxy was not finalized.')
    if request['preserveTimecode'] and video.get('tags', {}).get('timecode') != request['timecode']:
        raise RuntimeError('Timecode mismatch; proxy was not finalized.')
    if request['audioMode'] == 'preserve':
        def audio(path):
            data = probe(path, request)
            tracks = [s for s in data['streams'] if s['codec_type'] == 'audio']
            if not tracks:
                return ''
            return run_tool([request['ffmpeg'], '-v', 'error', '-i', path, '-map', '0:a', '-c', 'copy', '-f', 'streamhash', '-hash', 'sha256', '-'], request)
        if audio(request['source']) != audio(request['output']):
            raise RuntimeError('Source audio mismatch; proxy was not finalized.')
    return {'verified': True}


class ResolveBatch:
    """One isolated project and one scripting connection for the entire batch."""
    def __init__(self, request):
        self.request = request
        self.project = None
        self.lock = None
        self.previous_name = None
        self.name = 'DeProxy RAW ' + uuid.uuid4().hex
        self.entries = {}

    def __enter__(self):
        self.lock = open(Path(tempfile.gettempdir()) / 'deproxy-resolve-raw.lock', 'a')
        try:
            try:
                fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise RuntimeError("Another DeProxy RAW conversion is using Resolve.")
            self.resolve = connection()
            if self.request.get('action') == 'serve':
                # Do not mutate Resolve until the native parent acknowledges the connection.
                line = sys.stdin.readline()
                if not line or json.loads(line).get('action') != 'begin':
                    raise RuntimeError('RAW session was cancelled before preparation.')
            self.manager = self.resolve.GetProjectManager()
            previous = self.manager.GetCurrentProject()
            if not previous or previous.IsRenderingInProgress():
                raise RuntimeError('Open a Resolve project and wait for any active render to finish.')
            self.previous_name = previous.GetName()
            if not self.manager.SaveProject():
                raise RuntimeError('Could not save your current Resolve project; no project was switched.')
            self.project = self.manager.CreateProject(self.name)
            if not self.project:
                raise RuntimeError('Could not create the isolated RAW project.')
            check_cancel(self.request)
            if not self.project.SetSettings({'colorScienceMode': 'davinciYRGB'}):
                raise RuntimeError('Could not select ungraded RAW development.')
            pool = self.project.GetMediaPool()
            imported = pool.ImportMedia(self.request['sources'])
            if not imported or len(imported) != len(self.request['sources']):
                raise RuntimeError('Resolve could not import every RAW clip. Check camera support and source drive access.')
            for index, clip in enumerate(imported):
                check_cancel(self.request)
                props = clip.GetClipProperty()
                source = str(Path(props['File Path']).resolve())
                width, height = map(int, props['Resolution'].split('x'))
                render_width, render_height = dimensions(width, height, 1, self.request['outputHeight'])
                if not self.project.SetSettings({'timelineFrameRate': str(props['FPS']), 'timelineResolutionWidth': str(render_width), 'timelineResolutionHeight': str(render_height)}):
                    raise RuntimeError('Could not configure the RAW timeline.')
                if not clip.SetClipProperty('PAR', 'Square'):
                    raise RuntimeError('Could not set the intermediate to square pixels.')
                timeline = pool.CreateTimelineFromClips('RAW ' + str(index + 1), [clip])
                if not timeline or not timeline.SetStartTimecode(props['Start TC']):
                    raise RuntimeError('Could not create the RAW timeline with source timecode.')
                self.entries[source] = (index + 1, props, render_width, render_height)
            if set(self.entries) != {str(Path(p).resolve()) for p in self.request['sources']}:
                raise RuntimeError('Resolve imported unexpected source paths.')
            self.manager.SaveProject()
            # Commit the new project's timelines once, rather than reloading per clip.
            loaded = self.manager.LoadProject(self.name)
            if not loaded:
                raise RuntimeError('Could not open the prepared RAW project.')
            self.project = loaded
            self.resolve.OpenPage('deliver')
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def current(self):
        current = self.manager.GetCurrentProject()
        return current and current.GetName() == self.name

    def decode(self, request):
        if not self.current():
            raise RuntimeError('The active Resolve project changed. Cancel this batch before restarting.')
        check_cancel(request)
        index, props, width, height = self.entries[str(Path(request['source']).resolve())]
        project = self.project
        if not project.SetCurrentTimeline(project.GetTimelineByIndex(index)):
            raise RuntimeError('Could not select the RAW clip timeline.')
        job = None
        try:
            qualities = {'proxy': ('ProRes422P', 'Apple ProRes 422 Proxy'), 'lt': ('ProRes422LT', 'Apple ProRes 422 LT'), 'standard': ('ProRes422', 'Apple ProRes 422'), 'hq': ('ProRes422HQ', 'Apple ProRes 422 HQ')}
            quality = self.request.get('intermediateQuality', 'standard')
            if quality not in qualities:
                raise RuntimeError('Choose a supported RAW decoding quality.')
            codec, codec_name = qualities[quality]
            if not project.SetCurrentRenderMode(0) or not project.SetCurrentRenderFormatAndCodec('mov', codec):
                raise RuntimeError('The selected RAW decoding quality is unavailable in Resolve.')
            folder = str(Path(request['directory']).resolve())
            settings = {'TargetDir': folder, 'CustomName': 'decode', 'SelectAllFrames': True, 'ExportVideo': True, 'ExportAudio': False, 'FormatWidth': width, 'FormatHeight': height, 'FrameRate': float(props['FPS']), 'UseUniqueFilenames': True}
            if not project.SetRenderSettings(settings):
                raise RuntimeError('Resolve rejected the RAW render settings.')
            # Resolve's Deliver UI commits timeline/path changes asynchronously.
            self.manager.SaveProject()
            time.sleep(1)
            check_cancel(request)
            job = project.AddRenderJob()
            if not job:
                raise RuntimeError('Resolve refused the render path. Choose a writable output folder.')
            info = next(j for j in project.GetRenderJobList() if j['JobId'] == job)
            if info['VideoCodec'] != codec_name or info['FormatWidth'] != width or info['FormatHeight'] != height:
                raise RuntimeError('Resolve did not apply the requested render codec or resolution.')
            if not project.StartRendering([job]):
                raise RuntimeError('Resolve could not start RAW decoding.')
            deadline = time.monotonic() + max(600, int(props['Frames']) / float(props['FPS']) * 10)
            while project.IsRenderingInProgress():
                check_cancel(request)
                if not self.current():
                    raise RuntimeError('The active Resolve project changed during RAW decoding.')
                if time.monotonic() > deadline:
                    raise RuntimeError('Resolve RAW decoding timed out.')
                send(progress=project.GetRenderJobStatus(job).get('CompletionPercentage', 0))
                time.sleep(.25)
            if project.GetRenderJobStatus(job).get('JobStatus') != 'Complete':
                raise RuntimeError('Resolve RAW render failed or was stopped.')
            outputs = list(Path(folder).glob('decode*.mov'))
            if len(outputs) != 1:
                raise RuntimeError('Resolve did not produce one complete intermediate.')
            video = next(s for s in probe(outputs[0], request)['streams'] if s['codec_type'] == 'video')
            if video['codec_name'] != 'prores' or video.get('bits_per_raw_sample') != '10':
                raise RuntimeError('Resolve produced an unexpected intermediate codec.')
            source_width, source_height = map(int, props['Resolution'].split('x'))
            return {'project': self.name, 'input': str(outputs[0]), 'frames': int(props['Frames']), 'timecode': props['Start TC'], 'sourceWidth': source_width, 'sourceHeight': source_height}
        finally:
            if job and self.current():
                if project.IsRenderingInProgress():
                    project.StopRendering()
                    while project.IsRenderingInProgress():
                        time.sleep(.1)
                project.DeleteRenderJob(job)

    def __exit__(self, *_):
        try:
            if self.project and self.current():
                if self.project.IsRenderingInProgress():
                    self.project.StopRendering()
                    while self.project.IsRenderingInProgress():
                        time.sleep(.1)
                if not self.manager.LoadProject(self.previous_name):
                    raise RuntimeError('Could not restore your previous Resolve project. The isolated project is ' + self.name)
                self.manager.DeleteProject(self.name)
        finally:
            if self.lock:
                self.lock.close()
                self.lock = None


def decode(request):
    # Compatibility entry point for standalone validation.
    with ResolveBatch(dict(request, sources=[request['source']])) as batch:
        return batch.decode(request)


def serve(request):
    with ResolveBatch(request) as batch:
        send(result={'ready': True})
        for line in sys.stdin:
            command = json.loads(line)
            if command['action'] == 'close':
                break
            try:
                send(result=batch.decode(command))
            except Exception as error:
                send(error=str(error))
    return {'closed': True}


def main(request):
    if request['action'] == 'status':
        project = connection().GetProjectManager().GetCurrentProject()
        return {'project': project.GetName() if project else None}
    if request['action'] == 'serve':
        return serve(request)
    if request['action'] == 'inspect':
        return inspect(request)
    if request['action'] == 'decode':
        return decode(request)
    if request['action'] == 'verify':
        return verify(request)
    raise ValueError('Unknown RAW bridge action')


if __name__ == '__main__':
    try:
        send(result=main(json.loads(sys.argv[1])))
    except Exception as error:
        send(error=str(error))
        sys.exit(1)

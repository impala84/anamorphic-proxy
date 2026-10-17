import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch, Mock

spec = importlib.util.spec_from_file_location('raw', Path(__file__).with_name('resolve_raw.py'))
raw = importlib.util.module_from_spec(spec)
spec.loader.exec_module(raw)

class RawBridgeTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        patcher = patch.object(raw.tempfile, 'gettempdir', return_value=directory.name)
        patcher.start()
        self.addCleanup(patcher.stop)
        sleeping = patch.object(raw.time, 'sleep')
        sleeping.start()
        self.addCleanup(sleeping.stop)

    def test_mixed_clip_plan_uses_source_paths_not_filenames(self):
        with tempfile.TemporaryDirectory() as folder:
            first, second = [Path(folder) / name for name in ('anamorphic.CRM', 'spherical.crm')]
            first.touch(); second.touch()
            items = []
            for path, ratio in ((str(first),'1.5'),(str(second),'Square'),('/elsewhere/unrelated.mov','Unsupported unrelated PAR')):
                item=Mock();item.GetClipProperty.side_effect=lambda key,p=path,r=ratio: {'File Path':p,'PAR':r}[key];items.append(item)
            project=Mock();project.IsRenderingInProgress.return_value=False;project.GetMediaPool().GetRootFolder().GetClipList.return_value=items;project.GetMediaPool().GetRootFolder().GetSubFolderList.return_value=[];project.GetName.return_value='Film'
            resolve=Mock();resolve.GetProjectManager().GetCurrentProject.return_value=project
            with patch.object(raw,'connection',return_value=resolve), patch.object(raw.Path,'resolve',side_effect=AssertionError('Clip review must not resolve unrelated disk paths')):
                plan=raw.inspect({'sourceDir':folder,'squeeze':2,'useProjectAspect':True})
            self.assertEqual([x['squeeze'] for x in plan['clips']],[1.5,1.0])
            self.assertTrue(all(x['fromProject'] for x in plan['clips']))

    def test_invalid_aspect_is_not_silently_treated_as_square(self):
        for value in ('unsupported',0,float('nan'),float('inf')):
            with self.assertRaises(ValueError):raw.aspect(value)

    def test_four_k_geometry_keeps_full_desqueezed_frame(self):
        self.assertEqual(raw.dimensions(6000,3164,1.5,2160),(4096,1440))
        self.assertEqual(raw.dimensions(6000,3164,1,2160),(4096,2160))

    def test_cancel_before_import_restores_project(self):
        with tempfile.TemporaryDirectory() as folder:
            marker=Path(folder)/'cancel';marker.touch()
            resolve=Mock();manager=resolve.GetProjectManager();old=Mock();old.GetName.return_value='User film';old.IsRenderingInProgress.return_value=False
            isolated=Mock();isolated.GetName.return_value='owned';isolated.IsRenderingInProgress.return_value=False
            manager.GetCurrentProject.side_effect=[old,isolated,isolated];manager.CreateProject.return_value=isolated
            with patch.object(raw,'connection',return_value=resolve),patch.object(raw.uuid,'uuid4') as uid:
                uid.return_value.hex='test';isolated.GetName.return_value='DeProxy RAW test'
                with self.assertRaisesRegex(RuntimeError,'Cancelled'):raw.decode({'cancelPath':str(marker),'source':'test.CRM'})
            manager.LoadProject.assert_called_once_with('User film')
            isolated.StartRendering.assert_not_called()

    def test_existing_render_is_not_cancelled_or_project_switched(self):
        resolve=Mock();manager=resolve.GetProjectManager();manager.GetCurrentProject().IsRenderingInProgress.return_value=True
        with patch.object(raw,'connection',return_value=resolve):
            with self.assertRaisesRegex(RuntimeError,'active render'):raw.decode({'source':'test.CRM'})
        manager.CreateProject.assert_not_called()
        manager.GetCurrentProject().StopRendering.assert_not_called()

    def batch_fixture(self, folder):
        resolve = Mock(); manager = resolve.GetProjectManager()
        previous = Mock(); previous.GetName.return_value = 'User film'; previous.IsRenderingInProgress.return_value = False
        project = Mock(); project.GetName.return_value = 'DeProxy RAW test'; project.IsRenderingInProgress.return_value = False
        current = iter([previous])
        manager.GetCurrentProject.side_effect = lambda: next(current, project)
        manager.CreateProject.return_value = project; manager.LoadProject.return_value = project
        sources = [str(Path(folder)/name) for name in ('one.CRM','two.CRM')]
        items = []
        for source in sources:
            item = Mock(); item.GetClipProperty.return_value = {'File Path':source,'FPS':'29.97','Frames':'30','Resolution':'6000x3164','Start TC':'01:00:00:00'};items.append(item)
        project.GetMediaPool().ImportMedia.return_value = items
        project.AddRenderJob.return_value = 'owned-job'
        project.GetRenderJobList.return_value = [{'JobId':'owned-job','VideoCodec':'Apple ProRes 422','FormatWidth':4096,'FormatHeight':2160}]
        project.GetRenderJobStatus.return_value = {'JobStatus':'Complete'}
        Path(folder,'decode.mov').touch()
        request = {'sources':sources,'outputHeight':2160,'cancelPath':str(Path(folder)/'cancel')}
        return resolve, manager, project, request

    def test_two_clips_share_project_and_restore_only_at_batch_end(self):
        with tempfile.TemporaryDirectory() as folder:
            resolve, manager, project, request = self.batch_fixture(folder)
            with patch.object(raw,'connection',return_value=resolve), patch.object(raw.uuid,'uuid4') as uid, patch.object(raw,'probe',return_value={'streams':[{'codec_type':'video','codec_name':'prores','bits_per_raw_sample':'10'}]}):
                uid.return_value.hex = 'test'
                with raw.ResolveBatch(request) as batch:
                    results = [batch.decode(dict(request,source=source,directory=folder)) for source in request['sources']]
                    self.assertEqual([x['project'] for x in results], ['DeProxy RAW test']*2)
                    self.assertEqual(manager.LoadProject.call_count, 1)
                    manager.DeleteProject.assert_not_called()
                self.assertEqual(manager.LoadProject.call_count, 2)
                self.assertEqual(manager.LoadProject.call_args.args, ('User film',))
                manager.CreateProject.assert_called_once()
                project.GetMediaPool().ImportMedia.assert_called_once_with(request['sources'])
                self.assertEqual(project.SetCurrentTimeline.call_count, 2)
                self.assertEqual(project.DeleteRenderJob.call_count, 2)
                manager.DeleteProject.assert_called_once_with('DeProxy RAW test')

    def test_cancellation_between_clips_restores_once(self):
        with tempfile.TemporaryDirectory() as folder:
            resolve, manager, project, request = self.batch_fixture(folder)
            with patch.object(raw,'connection',return_value=resolve), patch.object(raw.uuid,'uuid4') as uid:
                uid.return_value.hex = 'test'
                with self.assertRaisesRegex(RuntimeError,'Cancelled'):
                    with raw.ResolveBatch(request) as batch:
                        Path(request['cancelPath']).touch()
                        batch.decode(dict(request,source=request['sources'][1],directory=folder))
                self.assertEqual(manager.LoadProject.call_count, 2)
                project.StartRendering.assert_not_called()

    def test_preparation_reload_failure_still_restores_user_project(self):
        with tempfile.TemporaryDirectory() as folder:
            resolve, manager, project, request = self.batch_fixture(folder)
            manager.LoadProject.side_effect = [None, Mock()]
            with patch.object(raw,'connection',return_value=resolve), patch.object(raw.uuid,'uuid4') as uid:
                uid.return_value.hex = 'test'
                with self.assertRaisesRegex(RuntimeError,'prepared RAW project'):
                    with raw.ResolveBatch(request):
                        self.fail('Failed preparation must not enter the batch')
            self.assertEqual(manager.LoadProject.call_args.args, ('User film',))
            manager.DeleteProject.assert_called_once_with('DeProxy RAW test')

    def test_raw_quality_controls_the_resolve_codec(self):
        for quality, codec, label in [('proxy','ProRes422P','Apple ProRes 422 Proxy'),('lt','ProRes422LT','Apple ProRes 422 LT'),('standard','ProRes422','Apple ProRes 422'),('hq','ProRes422HQ','Apple ProRes 422 HQ')]:
            with self.subTest(quality=quality), tempfile.TemporaryDirectory() as folder:
                resolve, manager, project, request = self.batch_fixture(folder)
                request['intermediateQuality'] = quality
                project.GetRenderJobList.return_value[0]['VideoCodec'] = label
                with patch.object(raw,'connection',return_value=resolve), patch.object(raw.uuid,'uuid4') as uid, patch.object(raw,'probe',return_value={'streams':[{'codec_type':'video','codec_name':'prores','bits_per_raw_sample':'10'}]}):
                    uid.return_value.hex = 'test'
                    with raw.ResolveBatch(request) as batch:
                        batch.decode(dict(request,source=request['sources'][0],directory=folder))
                    project.SetCurrentRenderFormatAndCodec.assert_called_once_with('mov',codec)

if __name__=='__main__':unittest.main()

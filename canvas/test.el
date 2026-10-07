;;; test.el --- Tests for canvas-demo -*- lexical-binding: t -*-

(require 'canvas-demo)
(require 'cl-lib)

(defun canvas-demo-test--wait-for-frame (renderer canvas)
  "Call `canvas-demo--present' until it copies a frame. Fail after 2 seconds."
  (let ((deadline (+ (float-time) 2)))
    (while (and (not (canvas-demo--present renderer canvas))
                (< (float-time) deadline))
      (sleep-for 0.01))
    (should (< (float-time) deadline))))

(ert-deftest canvas-demo::present-copies-new-frames ()
  (let* ((canvas (list 'image :type 'canvas :id 'canvas-demo-test :data-width 64 :data-height 32))
         (renderer (canvas-demo--start 64 32)))
    (unwind-protect
        (progn
          ;; Two frames in a row: the thread keeps rendering.
          (canvas-demo-test--wait-for-frame renderer canvas)
          (canvas-demo-test--wait-for-frame renderer canvas))
      (canvas-demo--stop-renderer renderer))))

(ert-deftest canvas-demo::present-skips-mismatched-canvas ()
  (let* ((canvas (list 'image :type 'canvas :id 'canvas-demo-test :data-width 10 :data-height 10))
         (renderer (canvas-demo--start 64 32)))
    (unwind-protect
        (progn
          (sleep-for 0.1)
          (should-not (canvas-demo--present renderer canvas)))
      (canvas-demo--stop-renderer renderer))))

(ert-deftest canvas-demo::kill-buffer-stops-animation ()
  (canvas-demo)
  (kill-buffer "*canvas-demo*")
  (should-not (cl-find #'canvas-demo--tick timer-list :key #'timer--function)))

;;; test.el ends here

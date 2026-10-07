;;; canvas-demo.el --- Animate a canvas from a Rust thread -*- lexical-binding: t -*-

;;; Commentary:

;; Requires Emacs 32 with a window system.  Run `M-x canvas-demo'.  Kill the buffer to stop.

;;; Code:

(require 'canvas-demo-dyn)

(defvar canvas-demo-width 480)
(defvar canvas-demo-height 270)
(defvar canvas-demo-fps 60)

(defvar-local canvas-demo--canvas nil)
(defvar-local canvas-demo--renderer nil)
(defvar-local canvas-demo--timer nil)

(defun canvas-demo ()
  "Show a canvas that a Rust thread animates."
  (interactive)
  (let ((buffer (get-buffer-create "*canvas-demo*")))
    (with-current-buffer buffer
      (canvas-demo--stop)
      (let ((inhibit-read-only t))
        (erase-buffer)
        ;; `list', not a quoted constant: each call needs a new spec, and so a new canvas.
        (setq canvas-demo--canvas (list 'image :type 'canvas :id 'canvas-demo
                                        :data-width canvas-demo-width
                                        :data-height canvas-demo-height))
        (insert (propertize " " 'display canvas-demo--canvas) "\n"))
      (setq buffer-read-only t)
      (setq canvas-demo--renderer (canvas-demo--start canvas-demo-width canvas-demo-height))
      (setq canvas-demo--timer
            (run-with-timer 0 (/ 1.0 canvas-demo-fps) #'canvas-demo--tick buffer))
      (add-hook 'kill-buffer-hook #'canvas-demo--stop nil t))
    (pop-to-buffer buffer)))

(defun canvas-demo--tick (buffer)
  "Copy the newest frame into the canvas of BUFFER."
  (if (buffer-live-p buffer)
      (with-current-buffer buffer
        (canvas-demo--present canvas-demo--renderer canvas-demo--canvas))
    (cancel-function-timers #'canvas-demo--tick)))

(defun canvas-demo--stop ()
  "Stop the animation in the current buffer."
  (when canvas-demo--timer
    (cancel-timer canvas-demo--timer)
    (setq canvas-demo--timer nil))
  (when canvas-demo--renderer
    (canvas-demo--stop-renderer canvas-demo--renderer)
    (setq canvas-demo--renderer nil)))

(provide 'canvas-demo)
;;; canvas-demo.el ends here

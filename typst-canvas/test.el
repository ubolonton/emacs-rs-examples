;;; test.el --- Tests for typst-canvas -*- lexical-binding: t -*-

(require 'typst-canvas)
(require 'cl-lib)
(require 'ert)

(defconst typst-canvas-test--root
  (file-name-directory (or load-file-name buffer-file-name)))

(defconst typst-canvas-test--timeout 60
  "Seconds to wait for a result.  The first compile scans the system fonts.")

(defconst typst-canvas-test--desk #x123456)

(defconst typst-canvas-test--page "#set page(width: 100pt, height: 50pt)\n"
  "Typst code that makes small pages, which render fast.")

(defun typst-canvas-test--wait (predicate)
  "Run timers and process filters until PREDICATE returns non-nil.
Fail after `typst-canvas-test--timeout' seconds."
  (let ((deadline (+ (float-time) typst-canvas-test--timeout)))
    (while (and (not (funcall predicate)) (< (float-time) deadline))
      (accept-process-output nil 0.05))
    (should (funcall predicate))))

(defun typst-canvas-test--call-with-session (function)
  "Call FUNCTION with a new session, and a function that counts notifications."
  (let* ((count 0)
         (process (make-pipe-process :name "typst-canvas-test" :noquery t :coding 'binary
                                     :filter (lambda (_process output)
                                               (cl-incf count (length output)))))
         (session (typst-canvas--session-start
                   typst-canvas-test--root
                   (expand-file-name "main.typ" typst-canvas-test--root)
                   process)))
    (unwind-protect
        (funcall function session (lambda () count))
      ;; Stop before deleting the process.  See `typst-canvas--stop'.
      (typst-canvas--session-stop session)
      (delete-process process))))

(defun typst-canvas-test--serve (session text &optional width)
  "Send TEXT to SESSION at WIDTH (default 300) pixels.  Wait until it is served.
Return the status."
  (let ((id (typst-canvas--session-request session text (or width 300) 1.0
                                           typst-canvas-test--desk nil nil)))
    (typst-canvas-test--wait (lambda () (>= (car (typst-canvas--session-status session)) id)))
    (typst-canvas--session-status session)))

(defun typst-canvas-test--canvas-for (session index)
  "Return a new canvas with the size of page INDEX of SESSION."
  (pcase-let ((`(,_serial ,width ,height . ,_) (typst-canvas--page-info session index)))
    (list 'image :type 'canvas :id (make-symbol "typst-canvas-test")
          :data-width width :data-height height)))

(ert-deftest typst-canvas::present-page-copies-pixels ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (typst-canvas-test--serve
      session "#set page(width: 100pt, height: 50pt, fill: rgb(\"#ff0000\"))")
     (let* ((canvas (typst-canvas-test--canvas-for session 0))
            (width (plist-get (cdr canvas) :data-width))
            (height (plist-get (cdr canvas) :data-height)))
       (should (= width 300))
       (should (typst-canvas--present-page session 0 canvas))
       ;; The margin has the desk color, the page its fill.
       (should (= (typst-canvas--canvas-pixel canvas 0 0)
                  (logior #xFF000000 typst-canvas-test--desk)))
       (should (= (typst-canvas--canvas-pixel canvas (/ width 2) (/ height 2)) #xFFFF0000))))))

(ert-deftest typst-canvas::present-page-skips-mismatched-canvas ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (typst-canvas-test--serve session typst-canvas-test--page)
     (let ((canvas (list 'image :type 'canvas :id (make-symbol "typst-canvas-test")
                         :data-width 10 :data-height 10)))
       (should-not (typst-canvas--present-page session 0 canvas))
       (should-not (typst-canvas--present-page session 1 canvas))))))

(ert-deftest typst-canvas::renders-each-page ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (pcase-let ((`(,_served ,pages ,errors . ,_)
                  (typst-canvas-test--serve
                   session (concat typst-canvas-test--page
                                   "A\n#pagebreak()\nB\n#pagebreak()\nC"))))
       (should (= pages 3))
       (should (= errors 0)))
     (let ((serials (mapcar (lambda (index) (car (typst-canvas--page-info session index)))
                            '(0 1 2))))
       (should (= (length (delete-dups (copy-sequence serials))) 3))
       (should-not (typst-canvas--page-info session 3))
       (dotimes (index 3)
         (should (typst-canvas--present-page session index
                                             (typst-canvas-test--canvas-for session index))))
       ;; Only the changed page gets a new image.
       (typst-canvas-test--serve
        session (concat typst-canvas-test--page "A\n#pagebreak()\nB2\n#pagebreak()\nC"))
       (should (equal (mapcar (lambda (index) (eql (car (typst-canvas--page-info session index))
                                                   (nth index serials)))
                              '(0 1 2))
                      '(t nil t)))))))

(ert-deftest typst-canvas::error-keeps-last-good-render ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (typst-canvas-test--serve session (concat typst-canvas-test--page "A"))
     (let ((serial (car (typst-canvas--page-info session 0))))
       ;; "é" is 2 bytes in UTF-8, but 1 char in Emacs.
       (pcase-let ((`(,_served ,pages ,errors . ,_)
                    (typst-canvas-test--serve session "é #nope")))
         (should (= pages 1))
         (should (= errors 1)))
       (should (eql (car (typst-canvas--page-info session 0)) serial))
       (pcase-let ((`((,beg ,end ,severity ,message))
                    (typst-canvas--session-diagnostics session)))
         (should (equal (list beg end severity) '(4 8 :error)))
         (should (string-search "unknown variable: nope" message))))
     ;; Fixing the error clears the diagnostics.
     (typst-canvas-test--serve session (concat typst-canvas-test--page "B"))
     (should-not (typst-canvas--session-diagnostics session)))))

(ert-deftest typst-canvas::view-request-re-renders-without-text ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (typst-canvas-test--serve session typst-canvas-test--page)
     (typst-canvas-test--serve session nil 400)
     (should (= (nth 1 (typst-canvas--page-info session 0)) 400)))))

(ert-deftest typst-canvas::notification-arrives-through-pipe ()
  (typst-canvas-test--call-with-session
   (lambda (session notifications)
     (should (= (funcall notifications) 0))
     (typst-canvas--session-request session typst-canvas-test--page 300 1.0 0 nil nil)
     (typst-canvas-test--wait (lambda () (> (funcall notifications) 0))))))

(ert-deftest typst-canvas::stop-ends-session ()
  (typst-canvas-test--call-with-session
   (lambda (session notifications)
     (typst-canvas-test--serve session typst-canvas-test--page)
     (typst-canvas-test--wait (lambda () (> (funcall notifications) 0)))
     (typst-canvas--session-stop session)
     (let ((served (car (typst-canvas--session-status session)))
           (count (funcall notifications)))
       (typst-canvas--session-request session "B" 300 1.0 0 nil nil)
       (accept-process-output nil 0.5)
       (should (= (car (typst-canvas--session-status session)) served))
       (should (= (funcall notifications) count))
       ;; Stopping again is harmless.
       (typst-canvas--session-stop session)))))

(ert-deftest typst-canvas::session-start-rejects-main-outside-root ()
  (let ((process (make-pipe-process :name "typst-canvas-test" :noquery t)))
    (unwind-protect
        (should-error (typst-canvas--session-start
                       (expand-file-name "src/" typst-canvas-test--root)
                       (expand-file-name "main.typ" typst-canvas-test--root)
                       process))
      (delete-process process))))

(ert-deftest typst-canvas::mode-shows-pages-and-diagnostics ()
  (let* ((temporary-file-directory (expand-file-name "target/" typst-canvas-test--root))
         (_ (make-directory temporary-file-directory t))
         (file (make-temp-file "typst-canvas-test" nil ".typ"
                               (concat typst-canvas-test--page "A\n#pagebreak()\nB")))
         (buffer (find-file-noselect file)))
    (unwind-protect
        (with-current-buffer buffer
          (typst-canvas-mode 1)
          (flymake-start)
          (let ((preview typst-canvas--preview))
            (with-current-buffer preview
              (typst-canvas-test--wait
               (lambda () (and (= (length typst-canvas--canvases) 2)
                               (cl-every #'identity typst-canvas--serials))))
              (should (equal (get-text-property (typst-canvas--page-position 1)
                                                'typst-canvas-page)
                             1))
              (should (eq (image-property (get-text-property (typst-canvas--page-position 1)
                                                             'display)
                                          :type)
                          'canvas))
              ;; `format-mode-line' returns "" in batch mode, so check the construct.  The
              ;; header line shows "%%" as "%".
              (let ((header (typst-canvas--header-line)))
                (should (string-search "2 pages" header))
                (should (string-search "100%%" header))))
            ;; An error goes to Flymake, and the preview keeps the last good pages.
            (goto-char (point-max))
            (insert "\n#nope")
            (typst-canvas-test--wait (lambda () (flymake-diagnostics)))
            (should (eq (flymake-diagnostic-type (car (flymake-diagnostics))) :error))
            (should (equal (buffer-substring (flymake-diagnostic-beg (car (flymake-diagnostics)))
                                             (flymake-diagnostic-end (car (flymake-diagnostics))))
                           "nope"))
            (with-current-buffer preview
              (should (= (length typst-canvas--canvases) 2))
              (should (string-search "1 error" (typst-canvas--header-line))))
            ;; A third page adds a line.
            (delete-region (- (point-max) 6) (point-max))
            (insert "\n#pagebreak()\nC")
            (with-current-buffer preview
              (typst-canvas-test--wait (lambda () (= (length typst-canvas--canvases) 3))))
            (typst-canvas-mode -1)
            (should-not (buffer-live-p preview))
            (should-not typst-canvas--process)
            (should-not typst-canvas--session)
            (should-not flymake-mode)))
      (with-current-buffer buffer
        (set-buffer-modified-p nil))
      (kill-buffer buffer)
      (delete-file file))))

(ert-deftest typst-canvas::killing-preview-turns-off-mode ()
  (with-temp-buffer
    (insert typst-canvas-test--page)
    (typst-canvas-mode 1)
    (let ((typst-buffer (current-buffer)))
      (kill-buffer typst-canvas--preview)
      (with-current-buffer typst-buffer
        (should-not typst-canvas-mode)
        (should-not typst-canvas--session)))))

;;;; Backward sync

(defconst typst-canvas-test--jump-page
  "#set page(width: 100pt, height: 100pt, margin: 10pt)\n"
  "Page setup whose first text line is at a known place.")

(defun typst-canvas-test--pixel (session page x y)
  "Return the pixel (X . Y) of the page point X, Y (in points) of PAGE of SESSION."
  (pcase-let ((`(,_serial ,_width ,_height ,page-x ,page-y ,scale)
               (typst-canvas--page-info session page)))
    (cons (floor (+ page-x (* x scale))) (floor (+ page-y (* y scale))))))

(ert-deftest typst-canvas::session-jump-finds-targets ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (let ((text (concat typst-canvas-test--jump-page
                         "é Hello\n\n#link(\"https://typst.app\")[Web]")))
       (typst-canvas-test--serve session text)
       (pcase-let ((`(,x . ,y) (typst-canvas-test--pixel session 0 10.5 15)))
         (should (equal (typst-canvas--session-jump session 0 x y)
                        (list 'source (1+ (string-search "é" text))))))
       ;; The margin leads nowhere.
       (should-not (typst-canvas--session-jump session 0 1 1))
       (pcase-let ((`(,x . ,y) (typst-canvas-test--pixel session 0 11 30)))
         (should (equal (typst-canvas--session-jump session 0 x y)
                        '(url "https://typst.app"))))))))

(ert-deftest typst-canvas::click-jumps-to-source ()
  (let* ((temporary-file-directory (expand-file-name "target/" typst-canvas-test--root))
         (_ (make-directory temporary-file-directory t))
         (file (make-temp-file "typst-canvas-test" nil ".typ"
                               (concat typst-canvas-test--jump-page
                                       "Hello world\n#place(horizon + center)[Middle]")))
         (buffer (find-file-noselect file)))
    (unwind-protect
        (with-current-buffer buffer
          (typst-canvas-mode 1)
          (goto-char (point-min))
          (let ((preview typst-canvas--preview)
                (session typst-canvas--session))
            (with-current-buffer preview
              (typst-canvas-test--wait
               (lambda () (and (= (length typst-canvas--serials) 1)
                               (aref typst-canvas--serials 0))))
              (should-not (typst-canvas--jump 0 1 1 'quiet))
              ;; A jump selects the source, so do it last here.
              (pcase-let ((`(,x . ,y) (typst-canvas-test--pixel session 0 11 15)))
                (should (typst-canvas--jump 0 x y))))
            ;; The jump selected the source, and moved point to the clicked word.
            (should (eq (current-buffer) buffer))
            (should (eq (window-buffer (selected-window)) buffer))
            (should (equal (thing-at-point 'word) "Hello"))
            ;; RET tries the middle of the visible part of the page.
            (goto-char (point-min))
            (with-current-buffer preview
              (goto-char (typst-canvas--page-position 0))
              (typst-canvas-jump-at-point))
            (should (equal (thing-at-point 'word) "Middle"))
            (typst-canvas-mode -1)))
      (with-current-buffer buffer
        (set-buffer-modified-p nil))
      (kill-buffer buffer)
      (delete-file file))))

;;;; Forward sync

(ert-deftest typst-canvas::caret-is-drawn-on-its-page ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (let ((text (concat typst-canvas-test--jump-page "A\n#pagebreak()\nHello")))
       (typst-canvas-test--serve session text)
       (pcase-let* ((`(,old ,new ,top ,bottom)
                     (typst-canvas--session-set-caret session (string-search "Hello" text)
                                                      #xff0000))
                    (canvas (typst-canvas-test--canvas-for session 1))
                    (`(,_serial ,_width ,_height ,page-x . ,_) (typst-canvas--page-info session 1))
                    (x (+ page-x 2))
                    (y (/ (+ top bottom) 2)))
         (should-not old)
         (should (= new 1))
         (should (< top bottom))
         (should (typst-canvas--present-page session 1 canvas))
         ;; The line band tints the page with the caret color.
         (let ((band (typst-canvas--canvas-pixel canvas x y)))
           (should (= (logand (ash band -16) #xff) #xff))
           (should (< (logand (ash band -8) #xff) #xff)))
         ;; Hiding the caret clears it on the next copy.
         (should (equal (typst-canvas--session-set-caret session nil 0) '(1 nil nil nil)))
         (should (typst-canvas--present-page session 1 canvas))
         (should (= (typst-canvas--canvas-pixel canvas x y) #xffffffff)))
       ;; No caret in code.
       (should-not (nth 1 (typst-canvas--session-set-caret
                           session (string-search "pagebreak" text) 0)))))))

(ert-deftest typst-canvas::caret-follows-point ()
  (with-temp-buffer
    (insert typst-canvas-test--jump-page "A\n#pagebreak()\nHello")
    (typst-canvas-mode 1)
    (unwind-protect
        (let ((preview typst-canvas--preview))
          (with-current-buffer preview
            (typst-canvas-test--wait
             (lambda () (and (= (length typst-canvas--serials) 2)
                             (cl-every #'identity typst-canvas--serials)))))
          (goto-char (point-max))
          (run-hooks 'post-command-hook)
          (typst-canvas-test--wait
           (lambda () (eql (buffer-local-value 'typst-canvas--caret-page preview) 1)))
          ;; In code, the caret disappears.
          (search-backward "pagebreak")
          (run-hooks 'post-command-hook)
          (typst-canvas-test--wait
           (lambda () (null (buffer-local-value 'typst-canvas--caret-page preview))))
          ;; With `typst-canvas-follow-cursor' off, too.
          (goto-char (point-max))
          (let ((typst-canvas-follow-cursor nil))
            (run-hooks 'post-command-hook)
            (accept-process-output nil (* 3 typst-canvas--follow-delay)))
          (should-not (buffer-local-value 'typst-canvas--caret-page preview)))
      (typst-canvas-mode -1))))

;;;; Theme

(ert-deftest typst-canvas::theme-colors-come-from-default-face ()
  (cl-letf (((symbol-function 'face-background) (lambda (&rest _) "#ffffff"))
            ((symbol-function 'face-foreground) (lambda (&rest _) "#202020")))
    (pcase-let ((`(,desk ,page ,ink) (typst-canvas--colors t)))
      (should (= page #xffffff))
      (should (= ink #x202020))
      ;; The desk is darker than a light page, so that the page stands out.
      (should (< desk page)))
    (should (equal (cdr (typst-canvas--colors nil)) '(nil nil))))
  (cl-letf (((symbol-function 'face-background) (lambda (&rest _) "#000000"))
            ((symbol-function 'face-foreground) (lambda (&rest _) "#ffffff")))
    ;; The desk is lighter than a black page.
    (should (> (car (typst-canvas--colors t)) 0))))

(ert-deftest typst-canvas::theme-colors-pages ()
  (typst-canvas-test--call-with-session
   (lambda (session _)
     (let ((id (typst-canvas--session-request session typst-canvas-test--page 300 1.0
                                              typst-canvas-test--desk #x000000 #xffffff)))
       (typst-canvas-test--wait (lambda () (>= (car (typst-canvas--session-status session)) id))))
     (let* ((canvas (typst-canvas-test--canvas-for session 0))
            (width (plist-get (cdr canvas) :data-width))
            (height (plist-get (cdr canvas) :data-height)))
       (should (typst-canvas--present-page session 0 canvas))
       (should (= (typst-canvas--canvas-pixel canvas (/ width 2) (/ height 2)) #xFF000000))))))

(ert-deftest typst-canvas::toggle-theme-sends-request ()
  (with-temp-buffer
    (insert typst-canvas-test--page)
    (typst-canvas-mode 1)
    (unwind-protect
        (let ((sent typst-canvas--sent))
          (with-current-buffer typst-canvas--preview
            (typst-canvas-toggle-theme)
            (should-not typst-canvas-match-theme))
          (should (> typst-canvas--sent sent))
          ;; The hooks are on while there is a session.
          (should (memq #'typst-canvas--on-theme-change enable-theme-functions)))
      (typst-canvas-mode -1))
    (should-not (memq #'typst-canvas--on-theme-change enable-theme-functions))))

;;; test.el ends here

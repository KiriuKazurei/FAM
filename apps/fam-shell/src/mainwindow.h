#ifndef MAINWINDOW_H
#define MAINWINDOW_H

#include <QElapsedTimer>
#include <QWidget>

#include <memory>

#include "bridge.h"
#include "pane.h"

class DriveList;
class IconCache;
class QComboBox;
class QLabel;
class QLineEdit;
class QPushButton;
class QTimer;
class NoteDialog;
class TagFilterBar;

class MainWindow final : public QWidget {
    Q_OBJECT

public:
    /* `configPath` empty means the platform config file; `initialQuery` runs a search once shown. */
    explicit MainWindow(const QString &configPath = QString(),
                        const QString &initialQuery = QString(), QWidget *parent = nullptr);

    ~MainWindow() override;

    /* Shows one directory and keeps it current: the core watches what a view is open on, so opening
     * is the whole of the subscription. Returns false with `error` set on failure. */
    bool openFolder(const QString &dir, QString *error);

    /* Tells the window which folder `--watch` asked for. A drive pick or an "Open folder…" dialog
     * changes what is on screen without changing that, so the headless timeline can still put the
     * watched tree in the other pane later in the run. */
    void noteWatchedFolder(const QString &dir) { m_watch = dir; }

    IconCache *icons() const { return m_icons; }

    /* What the run spent on the ABI, for the headless gate to read. */
    FamIpcCount ipcCount() const {
        return m_bridge != nullptr ? m_bridge->ipcCount() : FamIpcCount();
    }

    /* Drives the deepest column through several viewports and lets the paints settle. The headless
     * gate uses it to make the icon worker touch every extension in a folder before the HICON
     * counters are read: a leak is only visible after the code path has actually run. */
    void scrollForSelfCheck();

    /* One file in, one file out, on the folder the self-check is watching. Creating it is what makes
     * the core's reconciled re-read happen while a view is open, which is the only way the shell's
     * reconcile path gets run in a headless gate - and it has to be driven from inside the process,
     * because a harness that creates the file from outside cannot tell whether the run is still
     * alive. Opt-in: nothing writes into a folder the user pointed at unless asked. Removal undoes
     * every file the run wrote into the watched tree, whichever column each ended up in. */
    void createSelfCheckProbe();
    void removeSelfCheckProbe();

    /* The search box, driven from the headless timeline: starting a search while a folder is open,
     * and clearing the box to go back, are the two transitions a run with no hands on the keyboard
     * would otherwise never take. */
    void runSelfCheckSearch(const QString &terms);
    void clearSelfCheckSearch();

    /* Picks the first folder in the deepest column, which is what makes a second column appear. */
    void descendForSelfCheck();

    /* Clicks the deepest column's sort control once. */
    void sortDeepestForSelfCheck();

    /* Clicks the deepest column's header button through its signal, not by calling the sort. */
    void clickHeaderForSelfCheck();

    /* Picks the first reachable drive in the sidebar. */
    void pickFirstDriveForSelfCheck();

    /* Opens the second pane and puts a folder in it. Without this step the left pane is a widget that
     * was constructed and never shown, and the headless gate would be certifying one pane while
     * shipping two. */
    void toggleSplitForSelfCheck(bool on);

    /* The headless half of drag and drop: one file from one pane's folder into the other's, once with
     * no modifier and once with Ctrl held, then the same file copied back the other way. The handler
     * that runs is the one a person's drag runs - which is the point, since "the wiring works" and "the
     * file is where the modifier said" are two different claims and only the second is about the disk.
     * Opt-in like the probe, and it writes inside `--watch` only. */
    void prepareTransferForSelfCheck();
    void moveAcrossPanesForSelfCheck();
    void copyBackAcrossPanesForSelfCheck();
    /* A drop back into the folder the item is already in, which is the case the status line must not
     * turn into an error and the core must not be asked about at all. */
    void dropInPlaceForSelfCheck();

    /* The tag round trip on a file the run created: read what it carries, add one, read it back,
     * take it off again. Plus the two refusals that are the point of the dialog - a tag the store
     * rejects, and a remove with nothing picked, which on the wire means "delete every tag this file
     * has" and so must never be forwarded. Driven through the dialog's own operations, which is why
     * they are callable without showing it. */
    void tagsForSelfCheck();

    /* The same tags, read the way a row reads them: through the model's `TagsRole`, filled by the beat
     * rather than by the paint. Three claims, none of which the dialog step can make -
     * that the answer reaches the row at all, that a window nobody moved does not cost a second
     * crossing of the wire, and that taking the tag off takes the chip off. */
    void rowTagsForSelfCheck();

    /* The same rows as a picture, with six tags on the one file this run owns and a second column
     * opened beside it - narrower than six chips can fit, so the shot is the evidence that the band
     * counts its overflow instead of eating the name. The tags come back off before it returns. */
    bool grabRowTagsForSelfCheck(const QString &imagePath);

    /* The dialog itself, as an image. The step above proves the wire; only a picture says whether a
     * person can read what it lists, and a dialog is not part of the window the main grab takes.
     * Puts one tag on the probe file first so the list is not empty, and takes it off again after. */
    bool grabTagDialogForSelfCheck(const QString &imagePath);

    /* The vocabulary strip, driven the way a hand drives it: a chip is clicked, the query it writes
     * runs, and the results that come back carry the tagged file. Then the chip is clicked again and
     * the strip's own mark is the thing that clears the search. This is the step that exercises
     * `fam_tag_vocabulary` from its only interface caller. */
    void tagSearchForSelfCheck();

    /* The chip on a row, clicked the way a hand clicks it: the delegate's own hit test says where
     * the chip is, and a real press-and-release is posted there. Everything after the click is the
     * production path - the query written to the box, the search, the tagged file in the results. */
    void rowChipClickForSelfCheck();

    /* The note round trip on the probe file: none, written, read back, a blank save refused
     * without crossing the wire, cleared - and a second clear reporting there was nothing. Every
     * call goes through the dialog's own operations, which are the same ones a click reaches. */
    void notesForSelfCheck();
    /* The dialog as a picture, with a note in it - a new surface wears the theme or the picture
     * says so (increment 16's lesson). */
    bool grabNoteDialogForSelfCheck(const QString &imagePath);

    /* The strip itself, as an image: one chip checked and one not, so the two states a style sheet
     * has to tell apart are both in the frame. */
    bool grabTagFilterForSelfCheck(const QString &imagePath);

    /* Drops the deepest column, as Backspace would. */
    void goUpForSelfCheck();

protected:
    /* Unhandled key events reach here: Backspace and Alt+Left go up one column. */
    void keyPressEvent(QKeyEvent *event) override;

private slots:
    void runSearch();
    void pollChanges();
    void openFolderDialog();
    void tagSelectedFile();
    /* The free-text note on the selected row or folder - the note the agent could always read and
     * write, and the human face could not see at all until now. */
    void noteSelectedFile();
    /* A chip on a row was clicked: search that label, the same box-writing way the strip's chips
     * do. */
    void searchTagActivated(const QString &path, const QString &tag);
    /* The strip's context menu asked for a library-wide merge. */
    void mergeTags(const QString &from, const QString &into);
    /* Putting the folder back on screen: a search no longer closes the columns it was started in, so
     * clearing the box is what returns to them. */
    void showFolder();

private:
    void setStatus(const QString &text);
    /* One place words a directory header. The states are opposite claims that used to render the
     * same: a folder with nothing in it, a folder nobody has asked to read, and a folder that refuses
     * to be read (an ACL, a deleted path, a dead UNC share) all looked like a live, empty listing. */
    QString folderStatus(const QString &directory, const FamWindow &head,
                         const QString &extra = QString()) const;
    /* What a name search can answer with right now. Re-read rather than remembered: the index
     * backend's state is probed on the core's own thread and can change while the window is open. */
    QString searchBackendNote();
    /* One poll of a search that is still running: its row count grows, and when it stops growing the
     * rows are re-sorted, which is the last thing the model has to hear about. */
    void pollSearch(Pane *pane);
    void refreshScopeAvailability();
    /* The pane the toolbar and the keyboard act on. With two of them on screen, "open this folder"
     * and "run this search" each need one, and the only answer that is not a guess is the one the
     * person clicked last. */
    Pane *active() const { return m_active; }
    void setActivePane(Pane *pane);
    /* The same call with the pane named. `openFolder` opens in whichever pane is active, which is
     * right for a person and wrong for a gate that wants two folders side by side on purpose. */
    bool openFolderIn(Pane *pane, const QString &dir, QString *error);
    /* Shows or hides the left pane. Collapsed, the window is one browsing surface - the shape a
     * person who never asked for a second column has been getting all along. */
    void toggleSplit();
    /* Rebuilds the query from the strip's checked chips plus whatever name words are in the box,
     * then runs (or clears) the search. The box is the one place the query lives; this is the hand
     * that writes it when the gesture was a click. */
    void applyTagFilter();

    /* --- drag and drop ---------------------------------------------------------------
     *
     * A drop is the shell's only way for a pointer to ask for a write, and every rule about what it
     * means lives in these three: the destination is the folder the column shows (never the row the
     * point is over), the name is kept, and the transfer itself is the core's - so the same
     * never-overwrites rule that the MCP gate enforces is the one a person's drag runs into. */
    void handleDrop(const QString &folder, const QStringList &paths, bool copy);

    /* Posts a real drop event at a real column view, with the modifiers a person's hand would have
     * left on the keyboard, and answers with the action the view wrote back. It posts the drop rather
     * than driving a drag: the drag is a mouse gesture no headless run can perform, and everything
     * worth certifying - which folder, which name, copy or move, what refused - is decided here. */
    bool postDrop(FileView *view, const QString &source, bool copy, QString *outcome);
    /* Says whether the target view lets a foreign payload past its border. Two claims, both about the
     * decision not to have a second write path: ours is accepted, a standard `text/uri-list` is not. */
    void checkBorderForSelfCheck(FileView *view, const QString &source);
    /* Encodes a row that is actually listed the way a real drag would, and reads it back. */
    void reportDragPayload(FileView *view);
    /* One file from one pane's folder into the other's, then what the disk says about it. */
    bool dropBetweenPanes(const QString &from, const QString &to, bool copy);

    /* Two Windows paths name the same directory. `cleanPath` folds separators, duplicate separators,
     * `.` and a trailing slash; the case fold is Windows' rather than the filesystem's, because the
     * same folder opened by two panes with different spellings has to read as one place or a move
     * onto itself would be attempted. */
    static QString folderKey(const QString &path);

    QString deepestFolder() const { return m_active->chain->deepestPath(); }

    std::unique_ptr<FamBridge> m_bridge;
    /* False only when the core handle could not be created, which is the one case where the UI was
     * never built. `m_bridge` itself is never null after the constructor's first line, so testing it
     * is testing the wrong thing - and the destructor found out by dereferencing a null `m_chain`. */
    bool m_healthy = false;
    /* The volumes this machine has. Choosing one starts a chain from its root, in the active pane. */
    DriveList *m_drives = nullptr;
    /* The two panes. `m_left` is hidden until the split button asks for it; `m_right` always exists,
     * which is why the collapsed window is the same window it was before. */
    Pane *m_left = nullptr;
    Pane *m_right = nullptr;
    Pane *m_active = nullptr;
    QPushButton *m_split = nullptr;
    IconCache *m_icons = nullptr;
    QLineEdit *m_search = nullptr;
    QComboBox *m_kind = nullptr;
    /* Whether a search is scoped to the folder on screen or spans every configured root. Without it,
     * "search here" would be the only thing a search could do once a folder was open - and the
     * previous build could only search everywhere, so either one alone is a regression. */
    QComboBox *m_scope = nullptr;
    /* The library's vocabulary as clickable chips, under the toolbar controls. Absent while the
     * library holds no tags, which is why it is hidden at construction. */
    TagFilterBar *m_tagBar = nullptr;
    QLabel *m_status = nullptr;
    QTimer *m_timer = nullptr;
    QStringList m_kinds;
    /* Set while the opt-in self-check mutation is in the watched folder, so it can be undone. */
    QString m_probe;
    /* The store's file, and what it looked like at the last beat. An agent writes tags from another
     * process, and nothing inside this one would otherwise know - so the beat stats the file (and
     * its WAL, where a commit lands first) and a change buys one vocabulary re-read. */
    QString m_storeFile;
    qint64 m_storeStamp = -1;
    static qint64 storeStamp(const QString &file);
    /* The two ends of the headless transfer, both remembered because a copy leaves a file at each of
     * them and the cleanup step has to undo both. */
    QString m_draggedFrom;
    QString m_draggedTo;
    /* The folder `--watch` asked for, remembered so the headless timeline can put it in the *other*
     * pane and so prove the two are independent rather than two views of one chain. */
    QString m_watch;
    /* Everything the shell does after this point is measured against it, so the headless log can say
     * how long a folder took to show rather than leaving that to be guessed from a screenshot. */
    QElapsedTimer m_clock;
};

#endif /* MAINWINDOW_H */

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
     * alive. Opt-in: nothing writes into a folder the user pointed at unless asked. */
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
    QLabel *m_status = nullptr;
    QTimer *m_timer = nullptr;
    QStringList m_kinds;
    /* Set while the opt-in self-check mutation is in the watched folder, so it can be undone. */
    QString m_probe;
    /* The folder `--watch` asked for, remembered so the headless timeline can put it in the *other*
     * pane and so prove the two are independent rather than two views of one chain. */
    QString m_watch;
    /* Everything the shell does after this point is measured against it, so the headless log can say
     * how long a folder took to show rather than leaving that to be guessed from a screenshot. */
    QElapsedTimer m_clock;
};

#endif /* MAINWINDOW_H */
